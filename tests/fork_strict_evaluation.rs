//! A read that resolves to nothing is a coded failure, not `null`.
//!
//! The evaluator answered `null` for an identifier no binding provided, so a
//! typo in an expression became a confident wrong answer at exit 0 — the same
//! shape as the bash `$HOME` expansion (`tests/bash_compat_variables.rs`).
//! Both evaluator paths are covered here: the owned `Expr::Ident` arm and the
//! borrowed field-chain root in `borrowed_member` (the path `r.field` takes).
//!
//! What must *not* change is as important as what must:
//!
//! * an explicit `null`, and a variable bound to it, stay ordinary values;
//! * a call to an unknown function stays `E_UNKNOWN_BUILTIN`, and a missing
//!   record field stays `E_UNKNOWN_FIELD` — those names are wrong, not unbound;
//! * builtin *calls* and the module namespaces stay reachable, because a call
//!   and a dotted module path never read a bare name as a value;
//! * the static effect analysis is untouched: a read is not a call.

#![cfg(feature = "native")]

use aethershell::ast::Expr;
use aethershell::eval::{eval_expr, eval_program};
use aethershell::modules::env_with_modules;
use aethershell::parser::parse_program;
use aethershell::safety::{ErrorCode, SafetyError};
use aethershell::value::Value;

/// Evaluate `src` in a fresh module-bound environment.
fn run(src: &str) -> anyhow::Result<Value> {
    let program = parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    eval_program(&program, &mut env_with_modules())
}

/// The stable code of a structured failure.
fn code_of(e: &anyhow::Error) -> String {
    e.downcast_ref::<SafetyError>()
        .map(|se| se.code.as_str().to_string())
        .unwrap_or_else(|| panic!("not a structured failure: {e}"))
}

/// Evaluate `src` and return the `E_*` code its failure carries.
fn code_of_source(src: &str) -> String {
    code_of(&run(src).expect_err(&format!("`{src}` must fail")))
}

fn field<'a>(v: &'a Value, key: &str) -> &'a Value {
    match v {
        Value::Record(r) => r.get(key).unwrap_or_else(|| panic!("no field `{key}`")),
        other => panic!("expected a Record, got {other:?}"),
    }
}

// ── the read itself ─────────────────────────────────────────────────────

#[test]
fn an_unbound_read_is_a_coded_failure_not_null() {
    let e = run("missing_name").expect_err("an unbound read must not answer null");
    assert_eq!(code_of(&e), "E_UNBOUND_VARIABLE");

    // The message names the identifier, and the structured form carries the
    // repair rather than leaving an agent to retry the same read.
    let se = e.downcast_ref::<SafetyError>().expect("structured");
    assert_eq!(se.message, "undefined variable: missing_name");
    let json = se.to_json();
    assert_eq!(json["error"]["code"], "E_UNBOUND_VARIABLE");
    assert_eq!(json["error"]["retryable"], true);
    assert_eq!(json["error"]["expected"], "`missing_name` to be bound");
    assert!(
        json["error"]["hint"]
            .as_str()
            .is_some_and(|h| h.contains("explicit null")),
        "the hint must name both repairs: declare/pass it, or write null: {json}"
    );
}

#[test]
fn a_bound_null_and_an_explicit_null_are_still_values() {
    assert_eq!(run("null").unwrap(), Value::Null);
    assert_eq!(run("let x = null; x").unwrap(), Value::Null);
    assert_eq!(run("let x = null; x == null").unwrap(), Value::Bool(true));
    assert_eq!(run("type_of(null)").unwrap(), Value::Str("Null".into()));
    // A read of a *bound* null is a read like any other, and answers its value.
    assert_eq!(
        run("let x = null; [x]").unwrap(),
        Value::Array(vec![Value::Null])
    );
}

#[test]
fn a_read_in_a_lambda_body_fails_when_the_lambda_runs() {
    // The typo is in the body, so the failure arrives when the body runs rather
    // than when the lambda is built.
    assert_eq!(
        code_of_source("let threshold = 2\n[1, 2, 3] | where(fn(r) => r > treshold)"),
        "E_UNBOUND_VARIABLE"
    );
    // Non-vacuity: the same body with the name spelled right runs.
    assert_eq!(
        run("let threshold = 2\n[1, 2, 3] | where(fn(r) => r > threshold) | len").unwrap(),
        Value::Int(1)
    );
}

#[test]
fn a_read_that_roots_a_field_chain_names_the_name_not_a_null_receiver() {
    // The borrowed path: a chain rooted in a name resolves the root first, and
    // the receiver is what is wrong here — not the field.
    let e = run(r#"missing_module.read("x")"#).expect_err("an unbound module must fail");
    assert_eq!(code_of(&e), "E_UNBOUND_VARIABLE");
    assert!(
        e.to_string().contains("missing_module"),
        "the message must name the unbound receiver: {e}"
    );

    // The same chain inside a lambda body takes the same path.
    assert_eq!(
        code_of_source("[{n: 1}] | where(fn(r) => missing_module.n > r.n)"),
        "E_UNBOUND_VARIABLE"
    );
}

// ── names that are wrong in a different way ─────────────────────────────

#[test]
fn an_unknown_call_keeps_its_own_code() {
    // The callee of a call is not read as a value, so a bad builtin name is
    // still E_UNKNOWN_BUILTIN with its `did_you_mean` candidates.
    assert_eq!(
        code_of_source("no_such_builtin_here()"),
        "E_UNKNOWN_BUILTIN"
    );
    assert_eq!(
        code_of_source("[1, 2] | no_such_stage_here"),
        "E_UNKNOWN_BUILTIN"
    );
}

#[test]
fn a_missing_record_field_keeps_its_own_code() {
    assert_eq!(
        code_of_source("let r = {a: 1}\nr.b"),
        "E_UNKNOWN_FIELD",
        "a missing field is not an unbound name"
    );
}

// ── what a refused call may not do first ────────────────────────────────

#[test]
fn a_pipeline_source_fails_before_a_stage_body_runs() {
    // The source is evaluated before any stage sees an element. A stage body
    // that would itself fail with E_BAD_ARG is the proof: if the body had run,
    // that is the code that would have surfaced.
    assert_eq!(
        code_of_source("missing_source | map(fn(x) => 1 / 0)"),
        "E_UNBOUND_VARIABLE"
    );
    // Positive control: the body's own failure is what a bound source gives.
    assert_eq!(code_of_source("[1] | map(fn(x) => 1 / 0)"), "E_BAD_ARG");
}

#[test]
fn a_named_argument_is_refused_before_anything_runs() {
    // The grammar does not produce named arguments (see the test below), so the
    // AST is built by hand: `Call { named }` reaches the evaluator from any
    // direct AST caller.
    let mut env = env_with_modules();
    let named_call = Expr::Call {
        callee: Box::new(Expr::Ident("len".to_string())),
        // A positional argument that would fail as an unbound read: if the
        // positional arguments were evaluated first, this is the code that
        // would surface.
        args: vec![Expr::Ident("missing_positional".to_string())],
        named: vec![("value".to_string(), Expr::LitInt(1))],
    };
    let e = eval_expr(&named_call, &mut env).expect_err("named arguments must be refused");
    assert_eq!(code_of(&e), "E_BAD_ARG");
    assert!(
        e.to_string().contains("named argument(s): value"),
        "the refusal must name the dropped argument: {e}"
    );

    // Positive contrast: without the named entry, the same call reaches the
    // read, so the refusal above is the named-argument preflight and not the
    // positional argument failing on its own.
    let positional_only = Expr::Call {
        callee: Box::new(Expr::Ident("len".to_string())),
        args: vec![Expr::Ident("missing_positional".to_string())],
        named: Vec::new(),
    };
    assert_eq!(
        code_of(&eval_expr(&positional_only, &mut env).expect_err("the read fails")),
        "E_UNBOUND_VARIABLE"
    );
}

#[test]
fn the_grammar_has_no_named_argument_form() {
    // Refusing hand-built named arguments neither removes nor invents a source
    // form: this is why the positive control above is an AST.
    assert!(
        parse_program("len(value: 1)").is_err(),
        "the grammar gained a named-argument form; refusing `Call {{ named }}` \
         would then be rejecting something the parser produces"
    );
    assert!(
        parse_program("len(1)").is_ok(),
        "the ordinary call must parse"
    );
}

// ── bindings around a failed read ───────────────────────────────────────

#[test]
fn a_failed_read_restores_the_bindings_around_it() {
    let mut env = env_with_modules();
    env.set_var("outer", Value::Int(7)).expect("bind outer");
    env.set_var("e", Value::Str("outer".into()))
        .expect("bind e");

    // A lambda parameter shadows a caller binding for the duration of the call,
    // including when the body fails.
    let program = parse_program("let f = fn(outer) => missing_inside\nf(1)").expect("parses");
    let err = eval_program(&program, &mut env).expect_err("the body reads an unbound name");
    assert_eq!(code_of(&err), "E_UNBOUND_VARIABLE");
    assert_eq!(
        env.get_var("outer"),
        Some(&Value::Int(7)),
        "the parameter binding was not restored after the failure"
    );

    // A catch binding is installed for its handler and removed after, and does
    // not clobber a binding of another name.
    let program =
        parse_program("try { missing_here } catch err { err.error.code }").expect("parses");
    assert_eq!(
        eval_program(&program, &mut env).expect("the handler answers"),
        Value::Str("E_UNBOUND_VARIABLE".into())
    );
    assert_eq!(
        env.get_var("e"),
        Some(&Value::Str("outer".into())),
        "the catch binding clobbered an unrelated binding"
    );
    assert!(
        env.get_var("err").is_none(),
        "the catch binding outlived its handler"
    );
}

#[test]
fn an_unbound_read_stops_the_program_at_that_statement() {
    let mut env = env_with_modules();
    let program =
        parse_program("seen = \"before\"\nmissing_name\nafter = \"ran\"").expect("parses");
    let err = eval_program(&program, &mut env).expect_err("the read fails");

    assert_eq!(code_of(&err), "E_UNBOUND_VARIABLE");
    assert_eq!(
        env.get_var("seen"),
        Some(&Value::Str("before".into())),
        "the statement before the read did not run"
    );
    assert!(
        env.get_var("after").is_none(),
        "the statement after the failing read ran"
    );
}

#[test]
fn an_interpolation_hole_propagates_the_code() {
    // A hole is an expression, so it fails the way the same expression fails
    // outside the literal.
    assert_eq!(
        code_of_source(r#""value ${missing_hole}""#),
        "E_UNBOUND_VARIABLE"
    );
    assert_eq!(
        run(r#"let n = 1; "value ${n}""#).unwrap(),
        Value::Str("value 1".into())
    );
    // A raw literal has no holes: its `${...}` is text.
    assert_eq!(
        run(r#"r"value ${missing_hole}""#).unwrap(),
        Value::Str("value ${missing_hole}".into())
    );
}

// ── the reachability this must not break ────────────────────────────────

#[test]
fn a_builtin_name_is_a_call_and_a_module_is_a_binding() {
    // A call resolves its callee through the builtin dispatcher, not through a
    // variable read, so bare builtin names stay callable.
    assert_eq!(run("len([1, 2, 3])").unwrap(), Value::Int(3));
    assert_eq!(run("[1, 2, 3] | len").unwrap(), Value::Int(3));
    assert_eq!(
        run("[1, 2, 3] | map(fn(x) => x * 2) | reduce(fn(a, b) => a + b, 0)").unwrap(),
        Value::Int(12)
    );
    // A module namespace *is* an ordinary binding, so reading it as a value and
    // calling through it both keep working.
    assert_eq!(run(r#"str.upper("hi")"#).unwrap(), Value::Str("HI".into()));
    assert_eq!(
        run("let s = str\ns.upper(\"hi\")").unwrap(),
        Value::Str("HI".into())
    );
}

#[test]
fn a_bare_builtin_name_is_not_a_value() {
    // There are no first-class builtin values in the evaluator: `len` in value
    // position is a read of a variable named `len`. It used to answer `null`
    // and then fail later as "a function in `f`"; the read is the honest place
    // to stop.
    assert_eq!(code_of_source("let f = len"), "E_UNBOUND_VARIABLE");
}

// ── the taxonomy and the static analysis ────────────────────────────────

#[test]
fn the_new_code_carries_a_complete_contract() {
    let code = ErrorCode::UnboundVariable;
    assert_eq!(code.as_str(), "E_UNBOUND_VARIABLE");
    assert!(
        code.retryable(),
        "naming or declaring the variable is a correction the call can carry"
    );
    assert_eq!(
        code.exit_code(),
        127,
        "a name that does not resolve reads as bash's 127, like E_UNKNOWN_BUILTIN"
    );
    // Distinct from the neighbouring names-wrong codes.
    assert_ne!(code.as_str(), ErrorCode::UnknownBuiltin.as_str());
    assert_ne!(code.as_str(), ErrorCode::UnknownField.as_str());
    assert_ne!(code.as_str(), ErrorCode::BadArg.as_str());
}

#[test]
fn the_static_effect_analysis_is_unchanged_for_a_read() {
    // `explain_effects` never evaluates; a read is not a call, so the new
    // runtime failure adds no effect and no unresolved call.
    let explained = aethershell::explain::explain("missing_name").expect("static analysis");
    assert_eq!(field(&explained, "decision"), &Value::Str("allow".into()));
    assert_eq!(field(&explained, "effects"), &Value::Array(Vec::new()));
    assert_eq!(field(&explained, "unresolved"), &Value::Array(Vec::new()));
}

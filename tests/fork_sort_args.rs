//! A comparator is refused, not silently ignored.
//!
//! `sort` takes no argument beyond its reverse flags, so `sort(fn(a, b) => a - b)`
//! is refused by its declaration and an unknown flag is refused by the body,
//! rather than either being quietly dropped.
//!
//! `sort_by` is the call that does take a key function, and it takes a
//! *one-parameter* key: it calls that lambda once per element with the element.
//! A comparator written for it therefore fails on arity — and that failure has
//! to reach the caller. Swallowing it into a `Null` key gave every row the same
//! key, so the array came back in its original order and the call read as a sort
//! that had run. A key body that fails for any other reason must reach the
//! caller the same way.
//!
//! What must keep working is covered here too: `sort` with no argument, a
//! one-parameter key lambda, and a field name with `"desc"`. A field name that a
//! row does not carry still sorts rather than failing — that behaviour is
//! asserted by `tests/sort_by_key.rs`.

use aethershell::modules::env_with_modules;
use aethershell::value::Value;

fn eval(src: &str) -> Result<Value, String> {
    let mut env = env_with_modules();
    let program =
        aethershell::parser::parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, &mut env).map_err(|e| e.to_string())
}

fn ints(values: &[i64]) -> Value {
    Value::Array(values.iter().copied().map(Value::Int).collect())
}

fn strs(values: &[&str]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|s| Value::Str((*s).to_string()))
            .collect(),
    )
}

#[test]
fn sorting_without_an_argument_is_unchanged() {
    assert_eq!(eval("[3, 1, 2] | sort").expect("sort"), ints(&[1, 2, 3]));
    // A piped string is sorted as its lines, which is the oldest use of `sort`.
    assert_eq!(
        eval("\"b\\na\\nc\" | sort").expect("sort of a string"),
        strs(&["a", "b", "c"])
    );
}

#[test]
fn a_comparator_is_refused_rather_than_ignored() {
    let err = eval("[3, 1, 2] | sort(fn(a, b) => a - b)")
        .expect_err("a comparator is not a `sort` argument");
    assert!(
        err.contains("sort"),
        "the refusal does not name the call: {err}"
    );
    assert!(
        err.contains("E_BAD_ARG"),
        "the refusal is not the coded argument error an agent can branch on: {err}"
    );
}

#[test]
fn the_module_spelling_refuses_a_comparator_too() {
    let Value::Builtin(sorter) = eval("arr.sort").expect("module builtin reference") else {
        panic!("arr.sort must name a builtin");
    };
    let comparator = eval("fn(a, b) => a - b").expect("comparator literal");
    let mut env = env_with_modules();
    let err = aethershell::builtins::call_with_input(
        &sorter.name,
        vec![comparator],
        Some(ints(&[3, 1, 2])),
        &mut env,
    )
    .expect_err("a comparator is not an `arr.sort` argument")
    .to_string();
    assert!(
        err.contains("sort"),
        "the refusal does not name the call: {err}"
    );
    assert!(
        err.contains("E_BAD_ARG"),
        "the refusal is not the coded argument error an agent can branch on: {err}"
    );
}

#[test]
fn a_two_parameter_key_lambda_is_refused_rather_than_ignored() {
    // `sort_by`'s key is called with one element. A comparator takes two, and
    // that mismatch used to become a `Null` key for every row -- the input came
    // back unchanged and the call looked like it had sorted. The arity failure
    // now reaches the caller, naming the call and the count it wanted.
    let err = eval("[3, 1, 2] | sort_by(fn(a, b) => a - b)")
        .expect_err("a two-parameter key cannot be honoured");
    assert!(
        err.contains("expected 2 args, got 1"),
        "the failure does not say what the key got wrong: {err}"
    );
    assert!(
        err.contains("sort_by"),
        "the failure does not name the call: {err}"
    );
}

#[test]
fn a_failing_key_function_propagates_its_error() {
    // The key body's own failure must reach the caller too, rather than being
    // replaced by a `Null` key that hides it.
    let err = eval("[3, 1, 2] | sort_by(fn(x) => no_such_fork_builtin())")
        .expect_err("a key function that fails cannot produce a sort");
    assert!(
        err.contains("no_such_fork_builtin"),
        "the key's own failure was lost: {err}"
    );
    assert!(
        err.contains("E_UNKNOWN_BUILTIN"),
        "the key's own coded failure was lost: {err}"
    );
}

#[test]
fn a_key_lambda_and_a_descending_field_still_sort() {
    // Non-vacuity: the refusals above must come from a key call failing, not
    // from key functions or descending order being rejected everywhere.
    assert_eq!(
        eval("[3, 1, 2] | sort_by(fn(x) => 0 - x)").expect("a one-parameter key"),
        ints(&[3, 2, 1])
    );
    assert_eq!(
        eval(r#"[{n: 2}, {n: 1}] | sort_by("n", "desc") | map(fn(r) => r.n)"#).expect("desc"),
        ints(&[2, 1])
    );
}

#[test]
fn the_reverse_flags_sort_descending() {
    assert_eq!(
        eval(r#"[3, 1, 2] | sort("-r")"#).expect("sort -r"),
        ints(&[3, 2, 1])
    );
    assert_eq!(
        eval(r#"[3, 1, 2] | sort("--reverse")"#).expect("sort --reverse"),
        ints(&[3, 2, 1])
    );
    // The same flag said twice, or in both spellings, is still just `reverse`.
    assert_eq!(
        eval(r#"[3, 1, 2] | sort("-r", "-r")"#).expect("sort -r -r"),
        ints(&[3, 2, 1])
    );
    assert_eq!(
        eval(r#"[3, 1, 2] | sort("-r", "--reverse")"#).expect("sort -r --reverse"),
        ints(&[3, 2, 1])
    );
}

#[test]
fn an_unknown_flag_is_refused_rather_than_ignored() {
    // The flag list is variadic, so only this check separates a flag the
    // builtin honours from one it would otherwise drop and sort ascending by.
    for flag in ["-z", "nonsense", "--desc"] {
        let src = format!("[3, 1, 2] | sort(\"{flag}\")");
        let err = eval(&src).expect_err("an unknown flag cannot be honoured");
        assert!(
            err.contains("sort"),
            "the refusal does not name the call: {err}"
        );
        assert!(
            err.contains("E_BAD_ARG"),
            "the refusal is not the coded argument error an agent can branch on: {err}"
        );
        assert!(
            err.contains(flag),
            "the refusal does not repeat the flag it refused: {err}"
        );
    }
}

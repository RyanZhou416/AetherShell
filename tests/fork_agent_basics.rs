//! Batch-1 agent basics: `filter` is `where`, and `contains` reads arrays.
//!
//! Two practical spellings an agent reaches for were missing. `filter` is the
//! jq-style name and the array-module name (`arr.filter`), and it already ran
//! the same `bi_where` implementation through the array module — but under its
//! own name it was not in `BUILTIN_LOOKUP`, so `[1, 2, 3] | filter(...)` was
//! `E_UNKNOWN_BUILTIN`. `contains` answered `E_BAD_ARG: expected a string, got
//! Array` for `contains([1, 2, 3], 2)`, which is the one shape an agent writes
//! when it is not sure whether the subject is text or a collection.
//!
//! The two questions this file pins:
//!
//! - One canonical implementation, several honest spellings. `where` stays
//!   canonical; `filter` dispatches to the same index and resolves to the same
//!   declaration, so it cannot drift into a second implementation.
//! - `contains` keeps its string semantics exactly and adds array containment
//!   under the language's own typed `Value` equality — no stringification, so
//!   `1`, `1.0` and `"1"` are three different elements.

use aethershell::builtins::{call_with_input, is_dispatched, BUILTIN_LOOKUP};
use aethershell::modules::env_with_modules;
use aethershell::signature::signature_of;
use aethershell::value::Value;

fn eval(src: &str) -> Result<Value, String> {
    let mut env = env_with_modules();
    let program =
        aethershell::parser::parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, &mut env).map_err(|e| e.to_string())
}

fn ok(src: &str) -> Value {
    eval(src).unwrap_or_else(|e| panic!("`{src}` was refused: {e}"))
}

fn err(src: &str) -> String {
    match eval(src) {
        Ok(v) => panic!("`{src}` should have been refused, and answered {v:?}"),
        Err(e) => e,
    }
}

fn ints(values: &[i64]) -> Value {
    Value::Array(values.iter().copied().map(Value::Int).collect())
}

/// The first `E_...` code in an error's text.
fn error_code(msg: &str) -> Option<String> {
    let at = msg.find("E_")?;
    Some(
        msg[at..]
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || *c == '_')
            .collect(),
    )
}

fn bool_of(src: &str) -> bool {
    match ok(src) {
        Value::Bool(b) => b,
        other => panic!("`{src}` answered {other:?}, not Bool"),
    }
}

#[test]
fn filter_is_the_same_builtin_as_where() {
    // Both call forms an agent writes, and the canonical spelling unchanged.
    assert_eq!(ok("[1, 2, 3] | filter(fn(x) => x > 1)"), ints(&[2, 3]));
    assert_eq!(ok("filter([1, 2, 3], fn(x) => x > 1)"), ints(&[2, 3]));
    assert_eq!(ok("[1, 2, 3] | where(fn(x) => x > 1)"), ints(&[2, 3]));

    // One dispatch index, one declaration: `filter` is an alias, not a second
    // implementation that could drift from `where`.
    assert!(is_dispatched("filter"), "filter is not dispatched");
    assert_eq!(
        BUILTIN_LOOKUP.get("filter"),
        BUILTIN_LOOKUP.get("where"),
        "filter and where must share a dispatch index"
    );
    let declared = signature_of("filter").expect("filter must resolve to a declaration");
    assert_eq!(declared.name, "where", "filter declares its own signature");
    assert!(
        declared.aliases.contains(&"filter"),
        "where must declare filter: {:?}",
        declared.aliases
    );
}

#[test]
fn filter_keeps_the_where_contract() {
    // The predicate still decides, including when it keeps nothing...
    assert_eq!(
        ok("[1, 2, 3] | filter(fn(x) => x > 9)"),
        Value::Array(vec![])
    );
    // ...and a predicate that does not answer Bool fails the same way under
    // either spelling, because one body serves both.
    let via_where = err("[1, 2, 3] | where(fn(x) => 7)");
    let via_filter = err("[1, 2, 3] | filter(fn(x) => 7)");
    assert_eq!(
        error_code(&via_where),
        error_code(&via_filter),
        "the alias reports a different code than the canonical spelling:\n  where: {via_where}\n  filter: {via_filter}"
    );
    assert!(
        via_filter.contains("where predicate must return Bool"),
        "the refusal does not name the predicate contract: {via_filter}"
    );
    // The declaration still refuses a call with no predicate at all.
    assert!(err("filter([1, 2, 3])").contains("E_BAD_ARG"));
}

#[test]
fn the_array_module_spelling_reaches_the_same_builtin() {
    // `arr.filter` already existed; it is the reason `filter` was expected to
    // work as a bare name at all. It must name the canonical implementation.
    let Value::Builtin(filter) = ok("arr.filter") else {
        panic!("arr.filter must name a builtin");
    };
    assert_eq!(filter.name, "where");

    let lambda = ok("fn(x) => x > 1");
    let mut env = env_with_modules();
    let filtered = call_with_input(&filter.name, vec![lambda], Some(ints(&[1, 2, 3])), &mut env)
        .expect("arr.filter must filter");
    assert_eq!(filtered, ints(&[2, 3]));
}

#[test]
fn contains_reads_an_array_subject() {
    // Direct and piped, hit and miss.
    assert!(bool_of("contains([1, 2, 3], 2)"));
    assert!(!bool_of("[1, 2, 3] | contains(4)"));
    assert!(!bool_of("contains([], 1)"));

    // Arrays and records compare by value, deeply.
    assert!(bool_of("contains([[1, 2], [3]], [1, 2])"));
    assert!(!bool_of("contains([[1, 2], [3]], [2, 1])"));
    assert!(bool_of("contains([{a: 1}], {a: 1})"));
    assert!(!bool_of("contains([{a: 1}], {a: 2})"));
    assert!(bool_of("contains([null], null)"));

    // Typed equality: no stringification and no Int/Float mixing, so `1`,
    // `1.0` and `"1"` are three different elements.
    assert!(!bool_of("contains([1.0], 1)"));
    assert!(!bool_of("contains([1], 1.0)"));
    assert!(!bool_of(r#"contains(["1"], 1)"#));
    assert!(!bool_of(r#"contains([1], "1")"#));
}

#[test]
fn contains_string_behaviour_is_unchanged() {
    assert!(bool_of(r#""hello world" | contains("world")"#));
    assert!(bool_of(r#"contains("security fix", "security")"#));
    // The empty needle is still contained by every string, including the empty
    // one -- which is why the array case above reports a miss for `[]`.
    assert!(bool_of(r#""" | contains("")"#));
    assert!(bool_of(r#"contains("abc", "")"#));
    assert!(!bool_of(r#""abc" | contains("z")"#));

    // A string subject still refuses a needle that is not a string, rather
    // than stringifying it.
    let e = err("contains(\"abc\", 1)");
    assert!(e.contains("E_BAD_ARG"), "not the coded argument error: {e}");
    assert!(e.contains("contains"), "does not name the call: {e}");
}

#[test]
fn contains_refuses_a_subject_that_is_neither_string_nor_array() {
    for src in [
        "contains(1, 1)",
        "contains({a: 1}, 1)",
        "contains(true, true)",
        "contains(null, null)",
    ] {
        let e = err(src);
        assert!(e.contains("E_BAD_ARG"), "`{src}` is not coded: {e}");
        assert!(
            e.contains("String or Array"),
            "`{src}` does not name the accepted subjects: {e}"
        );
    }

    // The declared arity is exact: a direct call takes the subject and one
    // needle, a piped call takes only the needle.
    for src in [
        "contains([1, 2, 3])",
        "contains([1], 1, 2)",
        "[1, 2, 3] | contains()",
        "[1, 2, 3] | contains(1, 2)",
    ] {
        let e = err(src);
        assert!(e.contains("E_BAD_ARG"), "`{src}` is not coded: {e}");
    }
}

#[test]
fn ps_declares_the_process_record_fields() {
    let sig = signature_of("ps").expect("ps must be declared");
    assert_eq!(sig.name, "ps");
    assert_eq!(sig.aliases, &["proc_list"][..]);
    assert_eq!(sig.returns, "Array");
    assert!(
        sig.params.is_empty(),
        "ps takes no arguments: {}",
        sig.render()
    );
    assert_eq!(sig.category, Some("Process"));
    for field in ["pid", "name", "cpu", "mem", "command"] {
        assert!(
            sig.doc.contains(field),
            "ps's description omits the `{field}` field: {}",
            sig.doc
        );
    }
    assert!(
        sig.doc.contains("bytes") && sig.doc.contains("seconds"),
        "ps's description omits the units: {}",
        sig.doc
    );

    // Reachable under both spellings, and its record shape is discoverable
    // rather than the `Value` a name-derived entry would infer.
    assert!(is_dispatched("ps") && is_dispatched("proc_list"));
    let def = aethershell::agent_api::ontology_describe_json("ps");
    assert_eq!(def["category"], "Process");
    assert_eq!(def["return_type"], "Array");
    assert_eq!(def["signature"], "ps() -> Array");
    let aliases = def["aliases"].as_array().cloned().unwrap_or_default();
    assert!(
        aliases.iter().any(|a| a.as_str() == Some("proc_list")),
        "the ontology does not advertise the proc_list alias: {aliases:?}"
    );

    // A host reading, deliberately asserted only at the level of its type: the
    // process list itself is machine state.
    assert_eq!(ok("ps() | type_of"), Value::Str("Array".into()));
    // The declaration makes the argument-less shape explicit.
    assert!(err("ps(1)").contains("E_BAD_ARG"));
}

//! Strict field paths for the collection builtins: `Select-Object`/`select`,
//! `Where-Object`'s property mode, `Sort-Object`, `Group-Object`/`group_by`,
//! `Measure-Object`, and `sort_by`'s field form.
//!
//! Each took a field name and then answered as if it had used it: a name no row
//! carried became an empty string, a `Null` key, or the subject itself, and the
//! call reported success. Each case is refused now, and the calls that must keep
//! working sit beside the refusals: an explicit `null` is a value, counting
//! needs no field, and the lambda forms are untouched.

use std::collections::BTreeMap;

use aethershell::builtins::{call, call_with_input};
use aethershell::env::Env;
use aethershell::modules::env_with_modules;
use aethershell::value::{Table, Value};

fn arr(items: Vec<Value>) -> Value {
    Value::Array(items)
}

fn ints(values: &[i64]) -> Value {
    arr(values.iter().copied().map(Value::Int).collect())
}

fn map(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn rec(pairs: &[(&str, Value)]) -> Value {
    Value::Record(map(pairs))
}

fn text(name: &str) -> Value {
    Value::Str(name.to_string())
}

fn run(name: &str, args: Vec<Value>) -> Result<Value, String> {
    call(name, args, &mut Env::new()).map_err(|e| e.to_string())
}

fn piped(name: &str, args: Vec<Value>, input: Value) -> Result<Value, String> {
    call_with_input(name, args, Some(input), &mut Env::new()).map_err(|e| e.to_string())
}

fn lambda(src: &str) -> Value {
    let mut env = env_with_modules();
    let program = aethershell::parser::parse_program(src).expect("the lambda parses");
    aethershell::eval::eval_program(&program, &mut env).expect("the lambda evaluates")
}

fn field(value: &Value, key: &str) -> Value {
    match value {
        Value::Record(map) => map.get(key).cloned().expect("the field is present"),
        other => panic!("expected a Record, got {}", other.type_name()),
    }
}

fn rows(value: &Value) -> &[Value] {
    match value {
        Value::Array(items) => items,
        other => panic!("expected an Array, got {}", other.type_name()),
    }
}

// ── Select-Object / select ──────────────────────────────────────────────
#[test]
fn select_keeps_an_explicit_null_and_refuses_an_absent_field() {
    let kept = piped(
        "select-object",
        vec![text("x")],
        arr(vec![rec(&[("x", Value::Null)])]),
    )
    .expect("a field holding null is a field the record carries");
    assert_eq!(field(&rows(&kept)[0], "x"), Value::Null);
    let err = piped(
        "select-object",
        vec![text("x")],
        arr(vec![rec(&[("y", Value::Int(1))])]),
    )
    .expect_err("a field the record does not carry cannot be projected");
    assert!(err.contains("E_UNKNOWN_FIELD"), "coded refusal: {err}");
    assert!(err.contains("'x'"), "names the field: {err}");
    // The refusal carries the nearest field names, never the record's values.
    let secret = piped(
        "select-object",
        vec![text("nope")],
        arr(vec![rec(&[("token", Value::Str("s3cret-value".into()))])]),
    )
    .expect_err("an unknown field is refused");
    assert!(!secret.contains("s3cret-value"), "echoed a value: {secret}");
}

#[test]
fn select_refuses_a_bad_shape_before_it_reads_a_row() {
    // A non-String name over an empty subject proves the check precedes the rows.
    for input in [arr(vec![rec(&[("x", Value::Int(1))])]), arr(vec![])] {
        let err = piped("select-object", vec![Value::Int(5)], input).expect_err("not a name");
        assert!(err.contains("E_BAD_ARG") && err.contains("Int"), "{err}");
    }
    let scalar = piped("select-object", vec![text("x")], Value::Int(5))
        .expect_err("a scalar is not a record to project");
    assert!(
        scalar.contains("E_BAD_ARG") && scalar.contains("Int"),
        "{scalar}"
    );
    let row = piped("select-object", vec![text("x")], ints(&[1, 2]))
        .expect_err("an array of scalars has no fields to project");
    assert!(row.contains("E_BAD_ARG"), "{row}");
    // A Table is rows of records, projected row by row like `pick` projects one.
    let table = Value::Table(Table {
        rows: vec![map(&[("x", Value::Int(1)), ("y", Value::Int(2))])],
        schema: vec!["x".to_string(), "y".to_string()],
    });
    assert_eq!(
        piped("select-object", vec![text("x")], table).expect("a table has rows"),
        arr(vec![rec(&[("x", Value::Int(1))])])
    );
}

// ── Where-Object property mode ──────────────────────────────────────────
#[test]
fn an_unsupported_operator_is_refused_over_an_empty_subject() {
    let err = piped(
        "where-object",
        vec![text("x"), text("-bogus"), Value::Int(1)],
        arr(vec![]),
    )
    .expect_err("an operator the shell does not have cannot filter, empty or not");
    assert!(err.contains("E_BAD_ARG") && err.contains("-bogus"), "{err}");
    let empty = piped(
        "where-object",
        vec![text("x"), text("-eq"), Value::Int(1)],
        arr(vec![]),
    )
    .expect("a supported operator over no rows filters to no rows");
    assert_eq!(empty, arr(vec![]));
}

#[test]
fn a_comparison_refuses_a_pair_it_cannot_run_and_matches_one_it_can() {
    let like = piped(
        "where-object",
        vec![text("x"), text("-like"), Value::Int(1)],
        arr(vec![]),
    )
    .expect_err("`-like` matches a String pattern, checked before the rows");
    assert!(like.contains("E_BAD_ARG"), "{like}");
    let like = piped(
        "where-object",
        vec![text("n"), text("-like"), text("a")],
        arr(vec![rec(&[("n", Value::Int(1))])]),
    )
    .expect_err("a numeric field has no substring to match");
    assert!(like.contains("E_BAD_ARG") && like.contains("Int"), "{like}");
    let ordered = piped(
        "where-object",
        vec![text("s"), text("-gt"), Value::Int(1)],
        arr(vec![rec(&[("s", Value::Str("a".into()))])]),
    )
    .expect_err("a String field has no order against an Int target");
    assert!(ordered.contains("E_BAD_ARG"), "{ordered}");
    // Within the domain it still matches: Int against Float, and `-eq` identity.
    let numbers = arr(vec![
        rec(&[("n", Value::Int(5))]),
        rec(&[("n", Value::Int(9))]),
    ]);
    let kept = piped(
        "where-object",
        vec![text("n"), text("-gt"), Value::Float(6.0)],
        numbers,
    )
    .expect("an Int field against a Float target compares numerically");
    assert_eq!(field(&rows(&kept)[0], "n"), Value::Int(9));
    let flags = arr(vec![
        rec(&[("x", Value::Bool(true))]),
        rec(&[("x", Value::Bool(false))]),
    ]);
    let kept = piped(
        "where-object",
        vec![text("x"), text("-eq"), Value::Bool(true)],
        flags,
    )
    .expect("`-eq` compares any two values for identity");
    assert_eq!(rows(&kept).len(), 1);
}

#[test]
fn property_mode_refuses_a_fourth_argument_a_missing_field_and_keeps_lambdas() {
    let err = piped(
        "where-object",
        vec![text("x"), text("-eq"), Value::Int(1), text("-First")],
        arr(vec![rec(&[("x", Value::Int(1))])]),
    )
    .expect_err("property mode takes three arguments and no flag");
    assert!(err.contains("E_BAD_ARG"), "{err}");
    let err = piped(
        "where-object",
        vec![text("state"), text("-eq"), text("open")],
        arr(vec![
            rec(&[("state", Value::Str("open".into()))]),
            rec(&[("status", Value::Str("open".into()))]),
        ]),
    )
    .expect_err("a row that does not carry the property cannot match on it");
    assert!(
        err.contains("E_UNKNOWN_FIELD") && err.contains("state"),
        "{err}"
    );
    let subjects = arr(vec![
        rec(&[("x", Value::Int(1))]),
        rec(&[("x", Value::Int(2))]),
    ]);
    let one = piped(
        "where-object",
        vec![lambda("fn(r) => r.x == 1")],
        subjects.clone(),
    )
    .expect("a one-parameter predicate");
    assert_eq!(rows(&one).len(), 1);
    let indexed = piped("where-object", vec![lambda("fn(r, i) => i == 1")], subjects)
        .expect("a two-parameter predicate still receives the index");
    assert_eq!(field(&rows(&indexed)[0], "x"), Value::Int(2));
}

// ── Group-Object / group_by ─────────────────────────────────────────────
#[test]
fn group_refuses_a_row_without_the_property_and_keeps_typed_keys_distinct() {
    let err = piped(
        "group",
        vec![text("k")],
        arr(vec![
            rec(&[("k", Value::Null)]),
            rec(&[("other", Value::Int(1))]),
        ]),
    )
    .expect_err("a row without the property has no key to group under");
    assert!(err.contains("E_UNKNOWN_FIELD"), "{err}");
    // An explicit null is a key, and it is not the empty string.
    let grouped = piped(
        "group",
        vec![text("k")],
        arr(vec![
            rec(&[("k", Value::Null)]),
            rec(&[("k", Value::Str(String::new()))]),
        ]),
    )
    .expect("explicit keys are values, differently typed ones included");
    assert_eq!(rows(&grouped).len(), 2, "null merged with the empty string");
    assert!(rows(&grouped)
        .iter()
        .all(|g| field(g, "Count") == Value::Int(1)));
    let err = piped("group", vec![text("k")], ints(&[1, 2]))
        .expect_err("a scalar has no property to group on");
    assert!(err.contains("E_BAD_ARG") && err.contains("Record"), "{err}");
}

// ── Sort-Object ─────────────────────────────────────────────────────────
#[test]
fn sort_object_refuses_a_bad_property_and_still_sorts_present_ones() {
    let subject = arr(vec![
        rec(&[("size", Value::Int(1))]),
        rec(&[("name", Value::Str("x".into()))]),
    ]);
    let before = subject.clone();
    let err = piped("sort-object", vec![text("size")], subject.clone())
        .expect_err("a row without the sort property cannot be ordered by it");
    assert!(err.contains("E_UNKNOWN_FIELD"), "{err}");
    assert_eq!(subject, before, "the failed call changed its subject");
    for args in [vec![text("a"), text("b")], vec![Value::Int(1)]] {
        let err = piped("sort-object", args, arr(vec![]))
            .expect_err("the property shape is checked over an empty subject too");
        assert!(err.contains("E_BAD_ARG"), "{err}");
    }
    let err = piped("sort-object", vec![], Value::Int(5)).expect_err("only an array has an order");
    assert!(err.contains("E_BAD_ARG") && err.contains("Int"), "{err}");
    assert_eq!(
        piped("sort-object", vec![], ints(&[3, 1, 2])).expect("sort by value"),
        ints(&[1, 2, 3])
    );
    let mixed = piped(
        "sort-object",
        vec![],
        arr(vec![Value::Str("b".into()), Value::Int(3), Value::Null]),
    )
    .expect("the mixed-value ordering is unchanged");
    assert_eq!(rows(&mixed).len(), 3);
    let sorted = piped(
        "sort-object",
        vec![text("n")],
        arr(vec![
            rec(&[("n", Value::Int(2))]),
            rec(&[("n", Value::Int(1))]),
        ]),
    )
    .expect("sort by a property every row carries");
    assert_eq!(field(&rows(&sorted)[0], "n"), Value::Int(1));
}

// ── Measure-Object ──────────────────────────────────────────────────────
#[test]
fn measure_reads_a_property_strictly_and_counts_without_one() {
    let err = piped(
        "measure",
        vec![text("n")],
        arr(vec![
            rec(&[("n", Value::Int(2))]),
            rec(&[("n", Value::Str("x".into()))]),
        ]),
    )
    .expect_err("a Sum over the numeric rows alone is not the sum that was asked for");
    assert!(err.contains("E_BAD_ARG"), "{err}");
    let err = piped(
        "measure",
        vec![text("n")],
        arr(vec![rec(&[("m", Value::Int(2))])]),
    )
    .expect_err("a row without the property cannot be measured");
    assert!(err.contains("E_UNKNOWN_FIELD"), "{err}");
    let err = piped("measure", vec![Value::Int(1)], arr(vec![]))
        .expect_err("the property must be a name");
    assert!(err.contains("E_BAD_ARG"), "{err}");
    let err = piped("measure", vec![text("n")], Value::Int(5))
        .expect_err("measuring a property needs a collection to measure over");
    assert!(err.contains("E_BAD_ARG"), "{err}");
    let measured = piped(
        "measure",
        vec![text("n")],
        arr(vec![
            rec(&[("n", Value::Int(2))]),
            rec(&[("n", Value::Float(3.0))]),
        ]),
    )
    .expect("Int and Float rows measure together");
    assert_eq!(field(&measured, "Count"), Value::Int(2));
    assert_eq!(field(&measured, "Sum"), Value::Float(5.0));
    assert_eq!(field(&measured, "Average"), Value::Float(2.5));
    // Counting needs no field, whatever the elements are.
    let counted = piped(
        "measure",
        vec![],
        arr(vec![Value::Int(1), Value::Str("x".into()), Value::Null]),
    )
    .expect("counting needs no field");
    assert_eq!(field(&counted, "Count"), Value::Int(3));
    match counted {
        Value::Record(ref map) => assert!(!map.contains_key("Sum"), "counted a Sum: {map:?}"),
        other => panic!("expected a Record, got {}", other.type_name()),
    }
    // An empty measurement has a Count and no fabricated statistics.
    let empty = piped("measure", vec![text("n")], arr(vec![])).expect("an empty array has a count");
    match empty {
        Value::Record(map) => assert_eq!(
            map.keys().collect::<Vec<_>>(),
            vec!["Count"],
            "an empty measurement answered values it has no data for: {map:?}"
        ),
        other => panic!("expected a Record, got {}", other.type_name()),
    }
}

// ── sort_by's field form ────────────────────────────────────────────────
#[test]
fn sort_by_field_path_is_strict_and_its_lambda_form_is_unchanged() {
    let err = run(
        "sort_by",
        vec![
            arr(vec![
                rec(&[("n", Value::Int(1))]),
                rec(&[("m", Value::Int(2))]),
            ]),
            text("n"),
        ],
    )
    .expect_err("a row without the sort field cannot be ordered by it");
    assert!(err.contains("E_UNKNOWN_FIELD"), "{err}");
    let err = run("sort_by", vec![ints(&[2, 1]), text("n")])
        .expect_err("a scalar has no field to sort by");
    assert!(err.contains("E_BAD_ARG"), "{err}");
    let sorted = run(
        "sort_by",
        vec![
            arr(vec![
                rec(&[("n", Value::Int(2))]),
                rec(&[("n", Value::Int(1))]),
            ]),
            text("n"),
        ],
    )
    .expect("a field every row carries sorts");
    assert_eq!(field(&rows(&sorted)[0], "n"), Value::Int(1));
    let sorted = run(
        "sort_by",
        vec![
            arr(vec![
                rec(&[("n", Value::Int(3))]),
                rec(&[("n", Value::Int(1))]),
            ]),
            lambda("fn(r) => 0 - r.n"),
        ],
    )
    .expect("the lambda key form is untouched");
    assert_eq!(field(&rows(&sorted)[0], "n"), Value::Int(3));
}

#[test]
fn every_spelling_reaches_the_strict_path() {
    let ragged = || {
        arr(vec![
            rec(&[("k", Value::Int(1))]),
            rec(&[("other", Value::Int(2))]),
        ])
    };
    for name in [
        "Select-Object",
        "select-object",
        "select",
        "Group-Object",
        "group-object",
        "group",
        "group_by",
        "Sort-Object",
        "sort-object",
        "Measure-Object",
        "measure-object",
        "measure",
    ] {
        let err = piped(name, vec![text("k")], ragged()).expect_err("a missing field");
        assert!(err.contains("E_UNKNOWN_FIELD"), "{name}: {err}");
    }
    for name in ["Where-Object", "where-object"] {
        let err = piped(name, vec![text("k"), text("-eq"), Value::Int(1)], ragged())
            .expect_err("a missing field");
        assert!(err.contains("E_UNKNOWN_FIELD"), "{name}: {err}");
    }
}

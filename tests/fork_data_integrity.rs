//! Data-integrity contracts for `group_by`, the `min`/`max` aggregate,
//! `json_stringify` and the file patchers.
//!
//! Every assertion reaches the builtin through `builtins::call` and, where a
//! file is involved, reads the result back with `std::fs` — a builtin that
//! reported what it intended instead of what it produced cannot pass. The
//! fixtures are `tempfile::TempDir`s owned by the test. The process
//! environment is asserted, never changed: `AETHER_*` is process-global and
//! this binary runs its tests in parallel threads over one environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aethershell::builtins::{call, call_with_input, is_dispatched};
use aethershell::env::Env;
use aethershell::safety::{current_mode, effect_of, Effect, Mode};
use aethershell::value::Value;

// ── calling the builtins ────────────────────────────────────────────────

fn run(name: &str, args: Vec<Value>) -> Value {
    call(name, args, &mut Env::new()).unwrap_or_else(|e| panic!("{name} failed: {e}"))
}

fn piped(name: &str, input: Value) -> Value {
    call_with_input(name, vec![], Some(input), &mut Env::new())
        .unwrap_or_else(|e| panic!("{name} (piped) failed: {e}"))
}

fn refused(name: &str, args: Vec<Value>) -> String {
    match call(name, args, &mut Env::new()) {
        Ok(value) => panic!("{name} unexpectedly answered {value:?}"),
        Err(error) => error.to_string(),
    }
}

fn array(items: Vec<Value>) -> Value {
    Value::Array(items)
}

fn ints(xs: &[i64]) -> Value {
    array(xs.iter().map(|n| Value::Int(*n)).collect())
}

fn rec(pairs: &[(&str, Value)]) -> Value {
    let mut map = BTreeMap::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    Value::Record(map)
}

fn fields(value: &Value) -> &BTreeMap<String, Value> {
    match value {
        Value::Record(map) => map,
        other => panic!("expected a Record, got {}", other.type_name()),
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::Str(s) => s.clone(),
        other => panic!("expected a String, got {}", other.type_name()),
    }
}

fn number(result: &Value, key: &str) -> i64 {
    match fields(result).get(key) {
        Some(Value::Int(n)) => *n,
        other => panic!("`{key}` should be an Int, got {other:?}"),
    }
}

fn flag(result: &Value, key: &str) -> bool {
    match fields(result).get(key) {
        Some(Value::Bool(flag)) => *flag,
        other => panic!("`{key}` should be a Bool, got {other:?}"),
    }
}

// ── group_by ────────────────────────────────────────────────────────────

/// `(Name, Count, Group)` per group, in result order.
fn groups(value: &Value) -> Vec<(String, i64, Vec<Value>)> {
    let entries = match value {
        Value::Array(entries) => entries,
        other => panic!("expected an Array of groups, got {}", other.type_name()),
    };
    entries
        .iter()
        .map(|entry| {
            let entry = fields(entry);
            let name = text(entry.get("Name").expect("Name"));
            let count = match entry.get("Count").expect("Count") {
                Value::Int(n) => *n,
                other => panic!("Count should be an Int, got {}", other.type_name()),
            };
            let items = match entry.get("Group").expect("Group") {
                Value::Array(items) => items.clone(),
                other => panic!("Group should be an Array, got {}", other.type_name()),
            };
            (name, count, items)
        })
        .collect()
}

fn group_by(property: &str, items: Value) -> Vec<(String, i64, Vec<Value>)> {
    groups(
        &call_with_input(
            "group_by",
            vec![Value::Str(property.to_string())],
            Some(items),
            &mut Env::new(),
        )
        .unwrap_or_else(|e| panic!("group_by failed: {e}")),
    )
}

#[test]
fn structured_keys_group_separately() {
    // Distinct arrays are distinct groups even when they share a length, and
    // `Name` stays the display form callers already read.
    let arrays = array(vec![
        rec(&[("k", array(vec![Value::Int(1), Value::Int(2)]))]),
        rec(&[("k", array(vec![Value::Int(3), Value::Int(4)]))]),
    ]);
    let grouped = group_by("k", arrays);
    assert_eq!(
        grouped.len(),
        2,
        "distinct arrays merged into one group: {grouped:?}"
    );
    for (name, count, items) in &grouped {
        assert_eq!(
            name, "[len=2]",
            "the display label callers see is unchanged"
        );
        assert_eq!(*count, 1);
        assert_eq!(items.len(), 1);
    }

    let records = array(vec![
        rec(&[("k", rec(&[("a", Value::Int(1))]))]),
        rec(&[("k", rec(&[("a", Value::Int(2))]))]),
    ]);
    let grouped = group_by("k", records);
    assert_eq!(
        grouped.len(),
        2,
        "distinct records merged into one group: {grouped:?}"
    );
    assert!(grouped.iter().all(|(name, _, _)| name == "{…}"));
}

#[test]
fn scalar_keys_of_different_types_are_different_groups() {
    let cases: [(&str, Value, Value); 5] = [
        ("Int/Float", Value::Int(1), Value::Float(1.0)),
        ("Int", Value::Int(1), Value::Str("1".into())),
        ("Bool", Value::Bool(false), Value::Str("false".into())),
        ("Null", Value::Null, Value::Str("null".into())),
        ("Null", Value::Null, Value::Str("<null>".into())),
    ];
    for (label, left, right) in cases {
        let items = array(vec![
            rec(&[("k", left.clone())]),
            rec(&[("k", right.clone())]),
        ]);
        let grouped = group_by("k", items);
        assert_eq!(
            grouped.len(),
            2,
            "{label}: {left:?} and {right:?} merged into one group"
        );
        assert!(grouped.iter().all(|(_, count, _)| *count == 1));
        if label == "Int/Float" {
            assert!(grouped.iter().all(|(name, _, _)| name == "1"));
            let Value::Array(unique) = run("unique", vec![array(vec![left, right])]) else {
                panic!("unique must return an Array")
            };
            assert_eq!(unique.len(), 2);
        }
    }
}

#[test]
fn repeated_identical_values_share_one_group() {
    let pair = || array(vec![Value::Int(1), Value::Int(2)]);
    let items = array(vec![
        rec(&[("k", pair())]),
        rec(&[("k", pair())]),
        rec(&[("k", array(vec![Value::Int(3), Value::Int(4)]))]),
    ]);
    let grouped = group_by("k", items);
    assert_eq!(grouped.len(), 2);
    let counts: Vec<i64> = grouped.iter().map(|(_, count, _)| *count).collect();
    assert_eq!(
        counts,
        vec![2, 1],
        "identical keys must gather into one group exactly once"
    );
}

#[test]
fn group_by_key_identity_agrees_with_unique() {
    // `unique` is the identity oracle: as many distinct keys as it keeps, so
    // many groups `group_by` must produce.
    let keys = vec![
        Value::Int(1),
        Value::Str("1".into()),
        Value::Null,
        Value::Str("null".into()),
        Value::Bool(false),
        Value::Str("false".into()),
    ];
    let items = array(keys.iter().cloned().map(|k| rec(&[("k", k)])).collect());
    let distinct = match piped("unique", array(keys)) {
        Value::Array(items) => items,
        other => panic!("unique should answer an Array, got {}", other.type_name()),
    };
    assert_eq!(distinct.len(), 6);
    assert_eq!(group_by("k", items).len(), distinct.len());
}

#[test]
fn ordinary_string_keys_keep_the_display_name_and_their_order() {
    let items = array(vec![
        rec(&[("user", Value::Str("b".into()))]),
        rec(&[("user", Value::Str("a".into()))]),
        rec(&[("user", Value::Str("a".into()))]),
    ]);
    let grouped = group_by("user", items);
    assert_eq!(grouped.len(), 2);
    assert_eq!(grouped[0].0, "a");
    assert_eq!(grouped[0].1, 2);
    assert_eq!(grouped[1].0, "b");
    assert_eq!(grouped[1].1, 1);
}

// ── min / max: integers stay integers ───────────────────────────────────

#[test]
fn aggregate_extremum_keeps_integers_past_f64_precision() {
    let wide = ints(&[9_007_199_254_740_993, 9_007_199_254_740_992]);
    assert_eq!(
        run("max", vec![wide.clone()]),
        Value::Int(9_007_199_254_740_993)
    );
    assert_eq!(
        piped("max", wide.clone()),
        Value::Int(9_007_199_254_740_993)
    );
    assert_eq!(
        run("min", vec![wide.clone()]),
        Value::Int(9_007_199_254_740_992)
    );
    assert_eq!(piped("min", wide), Value::Int(9_007_199_254_740_992));

    let negative = ints(&[-9_007_199_254_740_993, -9_007_199_254_740_992]);
    assert_eq!(
        run("min", vec![negative.clone()]),
        Value::Int(-9_007_199_254_740_993)
    );
    assert_eq!(
        piped("min", negative.clone()),
        Value::Int(-9_007_199_254_740_993)
    );
    assert_eq!(
        run("max", vec![negative]),
        Value::Int(-9_007_199_254_740_992)
    );
}

#[test]
fn aggregate_extremum_handles_the_i64_bounds() {
    let span = ints(&[i64::MAX, i64::MIN]);
    assert_eq!(piped("max", span.clone()), Value::Int(i64::MAX));
    assert_eq!(piped("min", span), Value::Int(i64::MIN));

    let adjacent = ints(&[i64::MAX, i64::MAX - 1]);
    assert_eq!(piped("max", adjacent.clone()), Value::Int(i64::MAX));
    assert_eq!(piped("min", adjacent), Value::Int(i64::MAX - 1));
}

#[test]
fn the_two_number_form_and_the_aggregate_form_agree() {
    let pairs = [
        (9_007_199_254_740_993i64, 9_007_199_254_740_992i64),
        (i64::MIN, 0),
        (i64::MAX, 1),
    ];
    for (a, b) in pairs {
        assert_eq!(
            run("max", vec![Value::Int(a), Value::Int(b)]),
            piped("max", ints(&[a, b])),
            "max disagrees with itself for ({a}, {b})"
        );
        assert_eq!(
            run("min", vec![Value::Int(a), Value::Int(b)]),
            piped("min", ints(&[a, b])),
            "min disagrees with itself for ({a}, {b})"
        );
    }
}

#[test]
fn a_float_or_mixed_aggregate_still_uses_the_float_path() {
    assert_eq!(
        piped(
            "max",
            array(vec![Value::Int(1), Value::Float(5.5), Value::Int(3)])
        ),
        Value::Float(5.5)
    );
    assert_eq!(
        piped("min", array(vec![Value::Float(2.5), Value::Int(9)])),
        Value::Float(2.5)
    );
    // An all-Int aggregate still answers an Int, not a promoted Float.
    assert_eq!(piped("max", ints(&[1, 5, 3])), Value::Int(5));
}

#[test]
fn a_non_numeric_or_empty_aggregate_is_refused_by_name() {
    let message = refused("max", vec![array(vec![Value::Str("a".into())])]);
    assert!(message.contains("numeric"), "{message}");
    let message = refused("min", vec![Value::Array(vec![])]);
    assert!(message.contains("empty"), "{message}");
}

// ── json_stringify: non-finite floats ───────────────────────────────────

#[test]
fn json_stringify_renders_non_finite_floats_as_null() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            piped("json_stringify", Value::Float(value)),
            Value::Str("null".into()),
            "{value} did not serialize as JSON null"
        );
        assert_eq!(
            run("json_stringify", vec![Value::Float(value)]),
            Value::Str("null".into())
        );
    }
}

#[test]
fn non_finite_floats_inside_containers_become_json_null() {
    let value = rec(&[
        ("nan", Value::Float(f64::NAN)),
        (
            "list",
            array(vec![Value::Float(f64::INFINITY), Value::Float(1.5)]),
        ),
    ]);
    let rendered = text(&piped("json_stringify", value));
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
    assert!(parsed["nan"].is_null(), "{rendered}");
    assert!(parsed["list"][0].is_null(), "{rendered}");
    assert_eq!(parsed["list"][1].as_f64(), Some(1.5), "{rendered}");
}

#[test]
fn json_parse_round_trips_the_non_finite_rendering() {
    let rendered = piped("json_stringify", Value::Float(f64::NAN));
    assert_eq!(run("json_parse", vec![rendered]), Value::Null);
}

#[test]
fn opaque_integer_ids_beyond_i64_remain_distinct_as_strings() {
    let first = run(
        "json_parse",
        vec![Value::Str(r#""18446744073709551615""#.into())],
    );
    let second = run(
        "json_parse",
        vec![Value::Str(r#""18446744073709551614""#.into())],
    );
    assert_eq!(first, Value::Str("18446744073709551615".into()));
    assert_eq!(second, Value::Str("18446744073709551614".into()));
    assert_ne!(first, second);
}

#[test]
fn save_json_shares_the_non_finite_rendering() {
    let fixture = Fixture::new();
    let target = fixture.path("data.json");
    let value = rec(&[("n", Value::Float(f64::NAN))]);

    let result = run(
        "save_json",
        vec![Value::Str(target.to_string_lossy().into_owned()), value],
    );
    assert!(flag(&result, "success"));

    let written = std::fs::read_to_string(&target).expect("external read");
    let parsed: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
    assert!(parsed["n"].is_null(), "{written}");
}

// ── file fixtures ───────────────────────────────────────────────────────

/// The mode these tests describe. Asserted, not set: mode is a property of the
/// process, and this binary runs its tests in parallel threads.
fn assert_human_mode() {
    assert_eq!(
        current_mode(),
        Mode::Human,
        "the file patchers described here are default-allow in human mode; run \
         without AETHER_MODE or AETHER_AGENT set"
    );
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        assert_human_mode();
        Self {
            dir: tempfile::tempdir().expect("private fixture directory"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Every entry in the fixture directory, so a temporary file left behind by
    /// an aborted patch is visible.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path())
            .expect("readable fixture directory")
            .map(|entry| {
                entry
                    .expect("readable entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort_unstable();
        names
    }
}

fn patch(find: &str, replace: &str) -> Value {
    rec(&[
        ("find", Value::Str(find.to_string())),
        ("replace", Value::Str(replace.to_string())),
    ])
}

fn patch_file(path: &Path, patches: Vec<Value>) -> Value {
    run(
        "file_patch",
        vec![
            Value::Str(path.to_string_lossy().into_owned()),
            Value::Array(patches),
        ],
    )
}

fn failed_messages(result: &Value) -> Vec<String> {
    match fields(result).get("failed") {
        Some(Value::Array(entries)) => entries.iter().map(text).collect(),
        other => panic!("`failed` should be an Array, got {other:?}"),
    }
}

// ── file_patch: an empty find is a failure, not a write ─────────────────

#[test]
fn an_empty_find_is_refused_per_patch() {
    let fixture = Fixture::new();
    let target = fixture.path("target.txt");
    std::fs::write(&target, "alpha\n").expect("seed the fixture");

    let result = patch_file(&target, vec![patch("", "X")]);
    assert!(!flag(&result, "success"));
    assert_eq!(number(&result, "patches_applied"), 0);
    assert_eq!(number(&result, "patches_failed"), 1);
    let messages = failed_messages(&result);
    assert!(messages[0].contains("empty"), "{messages:?}");
}

#[test]
fn an_empty_find_never_reaches_the_file() {
    let fixture = Fixture::new();
    let target = fixture.path("interleave.txt");
    std::fs::write(&target, "abc").expect("seed the fixture");

    let result = patch_file(&target, vec![patch("", "X")]);
    assert!(!flag(&result, "success"));
    let after = std::fs::read_to_string(&target).expect("external read");
    assert_eq!(
        after, "abc",
        "the replacement was interleaved into the file"
    );
    assert!(!after.contains('X'), "{after}");
    assert_eq!(fixture.entries(), vec!["interleave.txt".to_string()]);
}

#[test]
fn a_patch_list_that_matches_nothing_leaves_the_target_alone() {
    let fixture = Fixture::new();
    let target = fixture.path("untouched.txt");
    let seeded = b"alpha\n";
    std::fs::write(&target, seeded).expect("seed the fixture");

    let result = patch_file(&target, vec![patch("zzz", "X")]);
    assert!(!flag(&result, "success"));
    assert_eq!(number(&result, "patches_applied"), 0);
    assert_eq!(number(&result, "patches_failed"), 1);
    assert_eq!(std::fs::read(&target).expect("external read"), seeded);
    assert_eq!(fixture.entries(), vec!["untouched.txt".to_string()]);
}

#[test]
fn valid_patches_around_a_refused_one_still_apply() {
    let fixture = Fixture::new();
    let target = fixture.path("mixed.txt");
    std::fs::write(&target, "one\ntwo\n").expect("seed the fixture");

    let result = patch_file(
        &target,
        vec![patch("one", "1"), patch("", "X"), patch("two", "2")],
    );
    assert!(!flag(&result, "success"));
    assert_eq!(number(&result, "patches_applied"), 2);
    assert_eq!(number(&result, "patches_failed"), 1);
    assert_eq!(std::fs::read(&target).expect("external read"), b"1\n2\n");
    assert_eq!(fixture.entries(), vec!["mixed.txt".to_string()]);
}

#[test]
fn a_whitespace_find_is_a_literal() {
    let fixture = Fixture::new();
    let target = fixture.path("spaces.txt");
    std::fs::write(&target, "a b\n").expect("seed the fixture");

    let result = patch_file(&target, vec![patch(" ", "_")]);
    assert!(flag(&result, "success"));
    assert_eq!(number(&result, "patches_applied"), 1);
    assert_eq!(std::fs::read(&target).expect("external read"), b"a_b\n");
}

#[test]
fn file_replace_reports_the_empty_needle_as_a_patch_failure() {
    let fixture = Fixture::new();
    let target = fixture.path("replace.txt");
    std::fs::write(&target, "alpha\n").expect("seed the fixture");

    let result = run(
        "file_replace",
        vec![
            Value::Str(target.to_string_lossy().into_owned()),
            Value::Str(String::new()),
            Value::Str("X".to_string()),
        ],
    );
    assert!(!flag(&result, "success"));
    assert_eq!(number(&result, "patches_applied"), 0);
    assert_eq!(number(&result, "patches_failed"), 1);
    assert_eq!(std::fs::read(&target).expect("external read"), b"alpha\n");
}

#[test]
fn an_empty_patch_list_is_a_no_op_success() {
    let fixture = Fixture::new();
    let target = fixture.path("noop.txt");
    std::fs::write(&target, "alpha\n").expect("seed the fixture");

    let result = patch_file(&target, vec![]);
    assert!(flag(&result, "success"));
    assert_eq!(number(&result, "patches_applied"), 0);
    assert_eq!(number(&result, "patches_failed"), 0);
    assert_eq!(std::fs::read(&target).expect("external read"), b"alpha\n");
    assert_eq!(fixture.entries(), vec!["noop.txt".to_string()]);
}

#[test]
fn a_zero_match_patch_is_still_a_write_effect() {
    // Skipping the write-back for a patch list that applied nothing must not
    // reclassify the builtin: the effect gate runs before the patches are read,
    // so an empty or all-failed list is still refused outside the workspace.
    assert!(matches!(effect_of("file_patch"), Effect::WriteLocal));
    assert!(matches!(effect_of("file_replace"), Effect::WriteLocal));
}

// ── non-vacuity ─────────────────────────────────────────────────────────

#[test]
fn non_vacuity_the_names_under_test_are_dispatched() {
    for name in [
        "group_by",
        "max",
        "min",
        "json_stringify",
        "save_json",
        "file_patch",
        "file_replace",
    ] {
        assert!(
            is_dispatched(name),
            "{name} is not dispatched; the tests above prove nothing"
        );
    }
}

#[test]
fn the_precision_and_null_contracts_cannot_regress() {
    assert_ne!(
        piped("max", ints(&[9_007_199_254_740_993, 9_007_199_254_740_992])),
        Value::Int(9_007_199_254_740_992),
        "the aggregate is rounding integers through f64 again"
    );
    assert_ne!(
        piped("json_stringify", Value::Float(f64::NAN)),
        Value::Str("0".into()),
        "a non-finite float is serializing as numeric zero again"
    );
}

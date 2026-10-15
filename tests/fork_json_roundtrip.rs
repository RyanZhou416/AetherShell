//! A null survives a JSON round trip.
//!
//! `json_to_value` maps JSON null to `Value::Null`, but the inverse left
//! `Value::Null` to `value_to_json`'s fallback arm, which renders a value with
//! `value_to_string` — and that renders Null as the text `<null>`. So a null
//! did not survive: `json_stringify(None)` returned the *string* `"<null>"`,
//! parsing it back produced a String rather than Null, and `save_json` wrote a
//! quoted placeholder into a file that any other JSON reader would take for
//! data.
//!
//! These are the shapes an agent reaches for: the bare builtins, the `json.*`
//! module spellings, and a real file on disk. Every assertion is on the value
//! space, so a string that merely *prints* like null cannot pass.

use aethershell::env::Env;
use aethershell::modules::env_with_modules;
use aethershell::value::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Evaluate `src` against an environment with every module namespace bound,
/// which is what `json.parse` and `json.stringify` need.
fn eval(src: &str) -> Value {
    let mut env = env_with_modules();
    eval_in(src, &mut env)
}

fn eval_in(src: &str, env: &mut Env) -> Value {
    let program =
        aethershell::parser::parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, env).unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
}

/// The value of `field`, panicking with the whole record when it is absent.
fn field<'a>(v: &'a Value, name: &str) -> &'a Value {
    match v {
        Value::Record(m) => m
            .get(name)
            .unwrap_or_else(|| panic!("no field `{name}` in {v:?}")),
        other => panic!("expected a Record, got {other:?}"),
    }
}

/// `{a: None, nested: [None, {b: None}]}` has a null at the top level, at the
/// top of an array, and inside a record inside that array.
fn assert_null_at_every_level(v: &Value) {
    assert!(
        matches!(field(v, "a"), Value::Null),
        "`a` is not Null: {:?}",
        field(v, "a")
    );
    let nested = field(v, "nested");
    let items = match nested {
        Value::Array(items) => items,
        other => panic!("`nested` is not an Array: {other:?}"),
    };
    assert_eq!(items.len(), 2, "`nested` lost an element: {nested:?}");
    assert!(
        matches!(items[0], Value::Null),
        "`nested[0]` is not Null: {:?}",
        items[0]
    );
    assert!(
        matches!(field(&items[1], "b"), Value::Null),
        "`nested[1].b` is not Null: {:?}",
        field(&items[1], "b")
    );
}

/// A private temporary directory, removed when it goes out of scope.
///
/// The name carries a wall-clock nonce plus a per-run counter, so a test binary
/// re-run or a second test in this file cannot land on the same directory —
/// and no real user path is written to.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "ae_fork_{tag}_{nanos:x}_{}",
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).expect("exclusively create the temporary directory");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_null_is_json_null_and_not_the_langle_null_text() {
    // `None` is the identifier an agent writes for no value; `null` is the
    // keyword spelling of the same thing. Both must reach JSON as `null`.
    assert_eq!(eval("json_stringify(None)"), Value::Str("null".to_string()));
    assert_eq!(eval("json_stringify(null)"), Value::Str("null".to_string()));
    // The `to_json` alias, with the value piped in rather than passed.
    assert_eq!(eval("None | to_json"), Value::Str("null".to_string()));
}

#[test]
fn the_strings_null_and_langle_null_are_not_touched() {
    assert_eq!(
        eval(r#"json_stringify("null")"#),
        Value::Str(r#""null""#.to_string())
    );
    assert_eq!(
        eval(r#"json_stringify("<null>")"#),
        Value::Str(r#""<null>""#.to_string())
    );
}

#[test]
fn a_null_a_word_null_and_a_langle_null_stay_distinct() {
    let source = r#"json_stringify({nothing: None, word: "null", text: "<null>"})"#;
    let text = match eval(source) {
        Value::Str(text) => text,
        other => panic!("json_stringify did not return a String: {other:?}"),
    };
    assert!(
        text.contains(r#""nothing":null"#),
        "the null field is not JSON null: {text}"
    );
    assert!(
        text.contains(r#""word":"null""#),
        "the string `null` changed: {text}"
    );
    assert!(
        text.contains(r#""text":"<null>""#),
        "the string `<null>` changed: {text}"
    );

    // Read the text back through the shell, with the document bound as a value
    // rather than pasted into source, so the test does not depend on any string
    // escaping the source form would need.
    let mut env = env_with_modules();
    env.set_var("fork_json_text", Value::Str(text))
        .expect("bind the JSON text");
    let back = eval_in("json_parse(fork_json_text)", &mut env);
    assert!(
        matches!(field(&back, "nothing"), Value::Null),
        "the null field did not come back as Null: {:?}",
        field(&back, "nothing")
    );
    assert_eq!(field(&back, "word"), &Value::Str("null".to_string()));
    assert_eq!(field(&back, "text"), &Value::Str("<null>".to_string()));
}

#[test]
fn null_survives_a_round_trip_at_every_level() {
    assert_null_at_every_level(&eval(
        r#"json_parse(json_stringify({a: None, nested: [None, {b: None}]}))"#,
    ));
}

#[test]
fn the_json_module_spellings_round_trip_the_same_nulls() {
    assert_null_at_every_level(&eval(
        r#"json.parse(json.stringify({a: None, nested: [None, {b: None}]}))"#,
    ));
}

#[test]
fn from_json_of_the_word_null_is_null_and_to_json_gives_it_back() {
    assert_eq!(eval(r#""null" | from_json"#), Value::Null);
    assert_eq!(
        eval(r#""null" | from_json | to_json"#),
        Value::Str("null".to_string())
    );
    // The same trip leaves a string that spells the placeholder alone.
    assert_eq!(
        eval(r#""\"<null>\"" | from_json | to_json"#),
        Value::Str(r#""<null>""#.to_string())
    );
}

#[test]
fn save_json_writes_json_null_to_a_file() {
    let dir = TempDir::new("json_roundtrip");
    let path = dir.path().join("roundtrip.json");

    let mut env = env_with_modules();
    env.set_var(
        "fork_json_path",
        Value::Str(path.to_string_lossy().into_owned()),
    )
    .expect("bind the output path");

    let written = eval_in(
        r#"save_json(fork_json_path, {a: None, nested: [None, {b: None}]})"#,
        &mut env,
    );
    assert_eq!(
        field(&written, "success"),
        &Value::Bool(true),
        "save_json did not report success: {written:?}"
    );

    let text = std::fs::read_to_string(&path).expect("save_json wrote the file");
    assert!(
        !text.contains("<null>"),
        "the file carries the placeholder text, not JSON null: {text}"
    );

    // An independent reader: whatever wrote the file, this is what any JSON
    // consumer sees in it.
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("the file is JSON");
    assert_eq!(parsed.get("a"), Some(&serde_json::Value::Null));
    assert_eq!(parsed.pointer("/nested/0"), Some(&serde_json::Value::Null));
    assert_eq!(
        parsed.pointer("/nested/1/b"),
        Some(&serde_json::Value::Null)
    );

    // And the shell reads its own file back as Null, not as a String.
    env.set_var("fork_json_text", Value::Str(text))
        .expect("bind the file text");
    assert_null_at_every_level(&eval_in("json_parse(fork_json_text)", &mut env));
}

//! Scalar-safe diagnostics: the `type_fields` value preview and the
//! `file_patch` missing-match message.
//!
//! Both surfaces cap a rendered string, and the cap counts Unicode scalars —
//! not bytes, and not grapheme clusters. The suffixes differ by surface: `…`
//! for `type_fields`, `...` for `file_patch`. A cap indexed by byte instead
//! panics on any character that straddles it, so every string a caller can
//! supply must reach the message intact.

use std::collections::BTreeMap;
use std::path::PathBuf;

use aethershell::builtins::call;
use aethershell::env::Env;
use aethershell::safety::{current_mode, Mode};
use aethershell::value::Value;

// ── calling the builtins ────────────────────────────────────────────────

fn run(name: &str, args: Vec<Value>) -> Value {
    call(name, args, &mut Env::new()).unwrap_or_else(|e| panic!("{name} failed: {e}"))
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

/// The `preview` field `type_fields` renders for a one-field record.
fn preview_for(value: Value) -> String {
    let result = run("type_fields", vec![rec(&[("field", value)])]);
    match &result {
        Value::Array(entries) => {
            let entry = entries.first().expect("one field produces one entry");
            text(fields(entry).get("preview").expect("preview"))
        }
        other => panic!(
            "type_fields should answer an array, got {}",
            other.type_name()
        ),
    }
}

// ── type_fields: the preview caps at fifty scalars ──────────────────────

#[test]
fn a_short_multibyte_string_keeps_every_scalar() {
    // 17 scalars, 51 bytes: past a byte cap of 50, inside the scalar cap.
    let sample = "中".repeat(17);
    let preview = preview_for(Value::Str(sample.clone()));
    assert!(preview.contains(&sample), "{preview}");
    assert!(
        !preview.contains('…'),
        "a value inside the cap must not be truncated: {preview}"
    );
}

#[test]
fn a_long_multibyte_string_is_capped_at_fifty_scalars() {
    let sample = "中".repeat(51);
    let head: String = sample.chars().take(50).collect();
    assert_eq!(preview_for(Value::Str(sample)), format!("\"{head}…\""));
}

#[test]
fn the_ascii_preview_style_is_unchanged() {
    let sample = "a".repeat(51);
    assert_eq!(
        preview_for(Value::Str(sample)),
        format!("\"{}…\"", "a".repeat(50))
    );
}

#[test]
fn emoji_are_capped_by_scalar_not_by_byte() {
    let sample = "😀".repeat(51);
    let head: String = sample.chars().take(50).collect();
    assert_eq!(preview_for(Value::Str(sample)), format!("\"{head}…\""));
}

#[test]
fn combining_marks_are_separate_scalars() {
    // 51 `e` + combining acute pairs: 102 scalars, and the cut may land
    // between a base character and its mark because the cap is not grapheme
    // aware.
    let sample = "e\u{0301}".repeat(51);
    let head: String = sample.chars().take(50).collect();
    assert_eq!(head.chars().count(), 50);
    assert_eq!(preview_for(Value::Str(sample)), format!("\"{head}…\""));
}

#[test]
fn mixed_scripts_cap_at_fifty_scalars() {
    let sample = "aé中😀e\u{0301}".repeat(20);
    let head: String = sample.chars().take(50).collect();
    assert_eq!(preview_for(Value::Str(sample)), format!("\"{head}…\""));
}

#[test]
fn preview_boundaries_never_cross_a_scalar() {
    // Around the cap for five alphabets, including one where a scalar is not a
    // `char::len_utf8` match for whole grapheme clusters.
    for unit in ["a", "é", "中", "😀", "e\u{0301}"] {
        for count in [49usize, 50, 51, 52] {
            let sample = unit.repeat(count);
            let preview = preview_for(Value::Str(sample.clone()));
            if sample.chars().count() > 50 {
                let head: String = sample.chars().take(50).collect();
                assert_eq!(preview, format!("\"{head}…\""), "{unit} x{count}");
            } else {
                assert!(preview.contains(&sample), "{unit} x{count}: {preview}");
                assert!(!preview.contains('…'), "{unit} x{count}: {preview}");
            }
        }
    }
}

// ── file_patch: the missing-match preview caps at forty scalars ─────────

fn assert_human_mode() {
    assert_eq!(
        current_mode(),
        Mode::Human,
        "the file patcher described here is default-allow in human mode; run \
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
}

/// The message a missing needle produces, capped by `file_patch` itself.
fn missing_needle_message(needle: &str) -> String {
    let fixture = Fixture::new();
    let target = fixture.path("target.txt");
    std::fs::write(&target, "nothing here\n").expect("seed the fixture");

    let result = run(
        "file_patch",
        vec![
            Value::Str(target.to_string_lossy().into_owned()),
            Value::Array(vec![rec(&[
                ("find", Value::Str(needle.to_string())),
                ("replace", Value::Str("x".to_string())),
            ])]),
        ],
    );
    match fields(&result).get("failed") {
        Some(Value::Array(entries)) => {
            assert_eq!(entries.len(), 1, "{result:?}");
            text(&entries[0])
        }
        other => panic!("`failed` should be an Array, got {other:?}"),
    }
}

#[test]
fn a_long_multibyte_needle_is_capped_at_forty_scalars() {
    let needle = "中".repeat(51);
    let head: String = needle.chars().take(40).collect();
    let message = missing_needle_message(&needle);
    assert!(message.contains(&format!("'{head}...'")), "{message}");
    assert!(
        !message.contains('…'),
        "the patch suffix stays ASCII: {message}"
    );
}

#[test]
fn a_forty_scalar_needle_is_reported_in_full() {
    // Exactly the cap: 40 scalars is 120 bytes for this alphabet.
    let needle = "中".repeat(40);
    let message = missing_needle_message(&needle);
    assert!(message.contains(&format!("'{needle}'")), "{message}");
    assert!(!message.contains("..."), "{message}");
}

#[test]
fn patch_previews_never_cross_a_scalar() {
    for unit in ["a", "中", "😀", "e\u{0301}"] {
        for count in [39usize, 40, 41, 42] {
            let needle = unit.repeat(count);
            let message = missing_needle_message(&needle);
            let expected = if needle.chars().count() > 40 {
                let head: String = needle.chars().take(40).collect();
                format!("'{head}...'")
            } else {
                format!("'{needle}'")
            };
            assert!(
                message.contains(&expected),
                "{unit} x{count}: expected {expected}, got {message}"
            );
        }
    }
}

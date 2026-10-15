//! Length has two explicit measures in this fork, and one that must not move.
//!
//! `len` counts `String::len` — UTF-8 bytes — and existing token and content
//! consumers size their buffers from it, so `len("你好") == 6`, `len("😀") == 4`
//! and `str.len` stay exactly as they were. The declaration described that
//! count as "characters", which is what this file also pins as corrected, and
//! two names now say which measure a caller wants:
//!
//! * `char_len` — Unicode **scalar values**, `chars().count()`. Not grapheme
//!   clusters: there is no segmentation, so a ZWJ emoji sequence counts every
//!   scalar it is written from and never the one glyph a UI draws.
//! * `byte_len` — UTF-8 bytes, the same number `len` gives a string.
//!
//! Both take a String subject, piped or positional, and both are `str.*`
//! members. Every declared example is parsed and evaluated here rather than
//! compared as text.

use aethershell::agent_api::ontology_describe_json;
use aethershell::builtins::{call, call_with_input, is_dispatched};
use aethershell::env::Env;
use aethershell::modules::{env_with_modules, str_module};
use aethershell::signature::{signature_of, Ty};
use aethershell::value::Value;

/// `é` decomposed: `e` then U+0301 COMBINING ACUTE ACCENT — two scalar values
/// in three UTF-8 bytes. An escape, because a bare combining mark is invisible
/// in a diff; the parser receives the real sequence, the AetherShell lexer
/// having no `\u` escape of its own.
const COMBINING: &str = "e\u{301}";

/// A family emoji from its scalars, so the U+200D ZERO WIDTH JOINERs are
/// visible: 5 scalar values, 18 UTF-8 bytes, one glyph in a UI that clusters.
const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";

fn value(src: &str) -> Value {
    let mut env = env_with_modules();
    let program =
        aethershell::parser::parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, &mut env)
        .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
}

/// `Value`'s Display writes ANSI colour unconditionally, so an example's result
/// is compared after stripping it — the same helper `declared_signatures` uses.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out.trim().trim_matches('"').to_string()
}

fn declared(name: &str) -> &'static aethershell::signature::Signature {
    assert!(is_dispatched(name), "{name} is not dispatched");
    signature_of(name)
        .unwrap_or_else(|| panic!("{name} has no declaration, so its entry comes from its name"))
}

#[test]
fn len_still_counts_utf8_bytes_and_says_so() {
    // The backward-compatibility pin, including a decomposition of it.
    assert_eq!(value(r#""你好" | len"#), Value::Int(6));
    assert_eq!(value("len(\"😀\")"), Value::Int(4));
    assert_eq!(value(r#""abc" | len"#), Value::Int(3));
    assert_eq!(value(&format!("len(\"{COMBINING}\")")), Value::Int(3));
    assert_eq!(value(r#"str.len("你好")"#), Value::Int(6));

    // The container counts are untouched.
    assert_eq!(value("len([1, 2, 3])"), Value::Int(3));
    assert_eq!(value("len({a: 1, b: 2})"), Value::Int(2));

    // And the description no longer promises the count it does not take.
    let sig = declared("len");
    assert_eq!(sig.returns, "Int");
    assert!(
        sig.doc.contains("UTF-8 bytes") && sig.doc.contains("not characters"),
        "len must explicitly distinguish byte length from character length: {}",
        sig.doc
    );
    assert!(
        sig.doc.contains("char_len"),
        "the description must point at the scalar measure: {}",
        sig.doc
    );
}

#[test]
fn char_len_counts_scalar_values_and_not_grapheme_clusters() {
    assert_eq!(value(r#""你好" | char_len"#), Value::Int(2));
    assert_eq!(value("char_len(\"😀\")"), Value::Int(1));
    assert_eq!(value("char_len(\"\")"), Value::Int(0));

    // Decomposed `é` is two scalars in three bytes; the composed spelling is one
    // scalar in two. The two strings look alike and these builtins say so.
    assert_eq!(value(&format!("char_len(\"{COMBINING}\")")), Value::Int(2));
    assert_eq!(value(&format!("byte_len(\"{COMBINING}\")")), Value::Int(3));
    assert_eq!(value("char_len(\"\u{e9}\")"), Value::Int(1));
    assert_eq!(value("byte_len(\"\u{e9}\")"), Value::Int(2));

    // One glyph in a UI, five scalars here. Nothing claims segmentation.
    assert_eq!(value(&format!("char_len(\"{FAMILY}\")")), Value::Int(5));
    assert_eq!(value(&format!("byte_len(\"{FAMILY}\")")), Value::Int(18));
    let sig = declared("char_len");
    assert!(
        sig.doc.contains("grapheme") && sig.doc.contains("scalar"),
        "the declaration must name the unit and the one it is not: {}",
        sig.doc
    );
}

#[test]
fn byte_len_counts_utf8_bytes_and_agrees_with_len() {
    assert_eq!(value("byte_len(\"你好\")"), Value::Int(6));
    assert_eq!(value("byte_len(\"😀\")"), Value::Int(4));
    assert_eq!(value("byte_len(\"\")"), Value::Int(0));
    for text in ["你好", "😀", "abc", "", COMBINING, FAMILY] {
        assert_eq!(
            value(&format!("byte_len(\"{text}\")")),
            value(&format!("len(\"{text}\")")),
            "byte_len and len disagree on {text:?}"
        );
    }
}

#[test]
fn the_declared_shape_is_a_string_subject_and_the_examples_run() {
    let mut checked = 0usize;
    for name in ["len", "char_len", "byte_len"] {
        let sig = declared(name);
        assert_eq!(sig.returns, "Int", "{name} returns a count");
        assert!(!sig.examples.is_empty(), "{name} declares no example");
        if name != "len" {
            assert_eq!(sig.subject, Some(Ty::Str), "{name}'s subject is a String");
            assert!(
                !sig.subject_required && sig.params.is_empty(),
                "{name} accepts its string positionally or piped, with no other parameter"
            );
            assert_eq!(sig.render(), format!("String | {name}() -> Int"));
        }

        // The catalogue entry is the declaration's, and its examples evaluate to
        // what they claim.
        let def = ontology_describe_json(name);
        assert_eq!(
            def["signature"].as_str(),
            Some(sig.render().as_str()),
            "{name}"
        );
        assert_eq!(def["return_type"].as_str(), Some("Int"), "{name}");
        for (code, want) in sig.examples {
            let got = value(code);
            assert_eq!(
                plain(&format!("{got}")),
                plain(want),
                "{name}'s example claims the wrong result: {code}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 8, "only {checked} examples were executed");
}

#[test]
fn the_module_spellings_are_the_same_builtins() {
    let Value::Record(members) = str_module() else {
        panic!("str_module() is not a record")
    };
    for name in ["char_len", "byte_len", "len"] {
        match members.get(name) {
            Some(Value::Builtin(b)) => assert_eq!(b.name, name, "str.{name} points elsewhere"),
            other => panic!("str.{name} is missing or not a builtin: {other:?}"),
        }
    }
    assert_eq!(value(r#"str.char_len("你好")"#), Value::Int(2));
    assert_eq!(value(r#"str.byte_len("你好")"#), Value::Int(6));
    assert_eq!(value(r#"str.len("你好")"#), Value::Int(6));
}

#[test]
fn a_non_string_is_refused_with_a_code() {
    for name in ["char_len", "byte_len"] {
        for bad in [
            Value::Int(123),
            Value::Array(vec![Value::Int(1)]),
            Value::Bool(true),
        ] {
            let error = call(name, vec![bad], &mut Env::new())
                .expect_err("a non-string argument must be refused");
            let msg = error.to_string();
            assert!(msg.contains("E_BAD_ARG"), "{name}: not coded: {msg}");
            assert!(!msg.contains("E_UNKNOWN"), "{name}: {msg}");
        }

        // A wrong-typed piped subject, and no argument at all, are refused the
        // same coded way rather than counting something.
        for (input, args) in [(Some(Value::Int(1)), vec![]), (None, vec![])] {
            let error = call_with_input(name, args, input, &mut Env::new())
                .expect_err("a call with no string must be refused");
            assert!(error.to_string().contains("E_BAD_ARG"), "{error}");
        }

        // Non-vacuity: the valid call is not refused.
        assert_eq!(
            call(name, vec![Value::Str("ab".into())], &mut Env::new()).expect("a valid call"),
            Value::Int(2)
        );
    }

    // And through the language, where an agent branches on the code. `char_len`
    // is spelled out because the negative call is the part worth reading.
    assert_eq!(
        value(r#"try { char_len(123) } catch e { e.error.code }"#),
        Value::Str("E_BAD_ARG".to_string())
    );
}

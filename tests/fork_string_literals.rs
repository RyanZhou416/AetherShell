//! Raw string literals (`r"..."`, `r#"..."#`) and the escape set an ordinary
//! double-quoted literal accepts.
//!
//! A raw literal is for text the language would otherwise rewrite: JSON,
//! regexes, Windows paths, and `${...}` templates addressed to something other
//! than AetherShell — a `save_json` payload, an HTTP body, a message another
//! tool interpolates. Its content is exactly the source between the
//! delimiters, it is never interpolated on either evaluator, and it types as
//! `String`.
//!
//! The same change refuses an unrecognized escape in an ordinary `"..."`:
//! `"C:\Users"` evaluated to `C:Users`, dropping the backslash without saying
//! so. Source is parsed in full before anything runs, so a refused program has
//! no effects.

#![cfg(feature = "native")]

use aethershell::explain::explain;
use aethershell::parser::{parse_program, ParseError};
use aethershell::types::Type;
use aethershell::value::Value;

fn eval(src: &str) -> Value {
    let stmts = parse_program(src).expect("source should parse");
    let mut env = aethershell::env::Env::new();
    aethershell::eval::eval_program(&stmts, &mut env).expect("source should evaluate")
}

/// The text of a source whose value is one string.
fn text(src: &str) -> String {
    match eval(src) {
        Value::Str(s) => s,
        other => panic!("expected a string, got {other:?}"),
    }
}

/// The structured failure a source is refused with.
fn refusal(src: &str) -> ParseError {
    let e = parse_program(src).expect_err("this source must not parse");
    e.downcast_ref::<ParseError>()
        .cloned()
        .unwrap_or_else(|| panic!("not a ParseError: {e}"))
}

/// One field of an `explain` report.
fn field(v: &Value, k: &str) -> Value {
    match v {
        Value::Record(r) => r.get(k).cloned().unwrap_or_else(|| panic!("no {k}")),
        _ => panic!("explain returns a record"),
    }
}

// ── what a raw literal preserves ────────────────────────────────────────

#[test]
fn a_windows_path_survives_verbatim() {
    assert_eq!(
        text(r#"r"C:\Users\dev\notes.txt""#),
        r"C:\Users\dev\notes.txt"
    );
}

#[test]
fn a_trailing_backslash_survives_verbatim() {
    assert_eq!(text(r#"r"C:\dir\""#), r"C:\dir\");
}

#[test]
fn a_regex_survives_verbatim() {
    assert_eq!(text(r#"r"\d+\.\d+""#), r"\d+\.\d+");
}

#[test]
fn a_hole_in_a_raw_literal_is_text_not_a_substitution() {
    // `x` is bound, so the ordinary form would substitute here.
    assert_eq!(text(r#"let x = 1; r"${x}""#), "${x}");
    assert_eq!(text(r#"r"${missing}""#), "${missing}");
}

#[test]
fn the_ordinary_form_still_interpolates() {
    assert_eq!(text(r#""${1 + 2}""#), "3");
    assert_eq!(text(r#"r"${1 + 2}""#), "${1 + 2}");
}

#[test]
fn an_interpolation_that_fails_now_refuses_and_the_raw_form_never_tries() {
    // The raw form has no hole to evaluate, so it is unchanged text.
    assert_eq!(text(r#"r"${1/0}""#), "${1/0}");

    // The ordinary form used to substitute `${1/0} [error: …]` into the
    // result, which reads as a successful interpolation of a string that looks
    // like a failure. The hole's own failure now reaches the caller with the
    // code the same expression carries outside a hole.
    let stmts = parse_program(r#""${1/0}""#).expect("the literal itself parses");
    let e = aethershell::eval::eval_program(&stmts, &mut aethershell::env::Env::new())
        .expect_err("a hole that fails must refuse the literal");
    let se = e
        .downcast_ref::<aethershell::safety::SafetyError>()
        .unwrap_or_else(|| panic!("not a structured failure: {e}"));
    assert_eq!(se.code.as_str(), "E_BAD_ARG", "{}", se.message);
    assert!(se.message.contains("division by zero"), "{}", se.message);
}

#[test]
fn a_raw_literal_spans_lines_and_keeps_indentation() {
    let src = "r\"first\n  indented\n\tthird\"";
    assert_eq!(text(src), "first\n  indented\n\tthird");
}

#[test]
fn a_raw_literal_keeps_crlf() {
    assert_eq!(text("r\"a\r\nb\""), "a\r\nb");
}

#[test]
fn a_raw_literal_keeps_unicode_and_an_embedded_byte_order_mark() {
    let content = "héllo ☃ 你好\u{feff}";
    assert_eq!(text(&format!("r\"{content}\"")), content);
}

// ── hash delimiters ─────────────────────────────────────────────────────

#[test]
fn hash_delimiters_carry_embedded_quotes() {
    assert_eq!(text(r##"r#"she said "hi""#"##), r#"she said "hi""#);
}

#[test]
fn a_shorter_hash_run_is_content_not_a_close() {
    assert_eq!(text(r###"r##"a"#b"##"###), r##"a"#b"##);
}

#[test]
fn at_most_sixteen_hash_delimiters() {
    let sixteen = format!("r{}\"a\"{}", "#".repeat(16), "#".repeat(16));
    assert_eq!(text(&sixteen), "a");

    let seventeen = format!("r{}\"a\"{}", "#".repeat(17), "#".repeat(17));
    let pe = refusal(&seventeen);
    assert!(pe.message.contains("17 '#' delimiters"), "{}", pe.message);
    assert!(pe.message.contains("maximum is 16"), "{}", pe.message);
}

#[test]
fn a_hash_left_over_after_a_close_is_refused() {
    let pe = refusal(r###"print(r#"a"##)"###);
    assert!(
        pe.message
            .contains("immediately after the raw string literal"),
        "{}",
        pe.message
    );
    // The same literal without the extra `#` is accepted.
    assert_eq!(text(r##"r#"a"#"##), "a");
}

// ── raw literals where any string is allowed ────────────────────────────

#[test]
fn a_matched_pattern_uses_the_same_text() {
    assert_eq!(
        text(r#"match r"${x}" { r"${x}" => "hit", _ => "miss" }"#),
        "hit"
    );
    assert_eq!(
        text(r#"match r"${x}" { "other" => "hit", _ => "miss" }"#),
        "miss"
    );
}

#[test]
fn a_raw_literal_types_as_string_even_when_it_looks_like_a_uri() {
    assert_eq!(
        aethershell::typecheck::infer_last_type(r#"r"https://example.com/x""#).expect("types"),
        Type::String
    );
    assert_eq!(
        aethershell::typecheck::infer_last_type(r#""https://example.com/x""#).expect("types"),
        Type::Uri
    );
}

#[test]
fn the_effect_analysis_sees_no_call_inside_a_raw_literal() {
    let called = explain(r#"rm("x")"#).expect("parses");
    let Value::Array(real) = field(&called, "calls") else {
        panic!("calls is an array")
    };
    assert!(!real.is_empty(), "a real call must be reported");

    let quoted = explain(r##"r#"${rm("x")}"#"##).expect("parses");
    let Value::Array(none) = field(&quoted, "calls") else {
        panic!("calls is an array")
    };
    assert!(none.is_empty(), "raw text is not a call site: {none:?}");
    assert_eq!(field(&quoted, "decision"), Value::Str("allow".into()));
}

// ── ordinary double-quoted escapes ──────────────────────────────────────

#[test]
fn the_ordinary_escape_set_is_backslash_quote_n_r_t() {
    assert_eq!(text(r#""a\\b""#), "a\\b");
    assert_eq!(text(r#""a\"b""#), "a\"b");
    assert_eq!(text(r#""a\nb""#), "a\nb");
    assert_eq!(text(r#""a\rb""#), "a\rb");
    assert_eq!(text(r#""a\tb""#), "a\tb");
}

#[test]
fn an_unknown_escape_is_refused_with_the_repair() {
    let pe = refusal(r#""C:\Users\""#);
    assert!(
        pe.message.contains("unknown escape sequence '\\U'"),
        "{}",
        pe.message
    );
    assert!(pe.message.contains("line 1, column 4"), "{}", pe.message);
    assert!(pe.hint.contains(r#"r"..."#), "{}", pe.hint);
    assert!(pe.hint.contains(r"\\"), "{}", pe.hint);
    assert!(pe.hint.contains("nothing ran"), "{}", pe.hint);
}

#[test]
fn an_unknown_escape_reports_the_backslash_line_and_column() {
    // The offending backslash is at line 3, column 10 of `let c = "\d+"`.
    let pe = refusal("let a = 1\nlet b = 2\nlet c = \"\\d+\"\n");
    assert!(pe.message.contains("line 3, column 10"), "{}", pe.message);
}

#[test]
fn an_unterminated_ordinary_quote_names_the_opening_position() {
    let pe = refusal("let a = \"abc");
    assert!(
        pe.message.contains("unterminated string literal"),
        "{}",
        pe.message
    );
    assert!(pe.message.contains("line 1, column 9"), "{}", pe.message);
}

#[test]
fn a_dangling_backslash_is_an_unterminated_escape() {
    let pe = refusal("let a = \"abc\\");
    assert!(
        pe.message.contains("unterminated escape sequence"),
        "{}",
        pe.message
    );
    assert!(pe.message.contains("line 1"), "{}", pe.message);
}

#[test]
fn an_unterminated_single_quote_names_the_opening_position() {
    let pe = refusal("let a = 'abc");
    assert!(
        pe.message.contains("unterminated string literal"),
        "{}",
        pe.message
    );
    assert!(pe.message.contains("line 1, column 9"), "{}", pe.message);
}

#[test]
fn single_quotes_still_keep_backslashes_and_interpolation() {
    assert_eq!(text(r"'a\b'"), r"a\b");
    assert_eq!(text("'${1 + 2}'"), "3");
}

#[test]
fn an_unterminated_raw_quote_names_the_opening_position() {
    let pe = refusal(r##"let a = r#"abc"##);
    assert!(
        pe.message.contains("unterminated raw string literal"),
        "{}",
        pe.message
    );
    assert!(pe.message.contains("line 1, column 9"), "{}", pe.message);
    assert!(pe.message.contains("1 '#' delimiter"), "{}", pe.message);
}

// ── `r` stays a name, and text stays inert ──────────────────────────────

#[test]
fn r_is_still_an_ordinary_identifier() {
    assert_eq!(eval("let r = 42; r"), Value::Int(42));
    assert_eq!(eval("let r1 = 7; r1"), Value::Int(7));
    assert_eq!(text("let r = fn(s) => s + \"!\"; r('value')"), "value!");
}

#[test]
fn a_hash_after_a_bare_r_is_a_comment_not_a_raw_literal() {
    // `r#abc` is the identifier `r` followed by a `#` comment; there is no raw
    // literal without an opening quote.
    assert_eq!(eval("let r = 5; r#abc"), Value::Int(5));
}

#[test]
fn raw_text_that_spells_a_call_stays_text() {
    let src = r##"r#"${save_json("private-path.json", 1)}"#"##;
    assert_eq!(text(src), r#"${save_json("private-path.json", 1)}"#);
}

#[test]
fn a_missing_closing_quote_refuses_the_whole_program_before_it_runs() {
    // The first statement is a write; the refusal comes from the lexer, so
    // nothing reached the evaluator and no path was touched.
    let e = parse_program("save_json(\"private-path.json\", 1); \"unterminated")
        .expect_err("the whole program must be refused");
    assert!(e.downcast_ref::<ParseError>().is_some(), "{e}");
}

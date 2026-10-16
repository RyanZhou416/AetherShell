//! Interpolation delimiters, whole-literal syntax preflight and propagated errors.
//! Quoted strings, raw literals and comments inside a hole keep their braces.
//! Raw-literal behavior is covered separately by `fork_string_literals`.

#![cfg(feature = "native")]

use aethershell::agent_api::{process_request, AgentRequest};
use aethershell::modules::env_with_modules;
use aethershell::parser::{parse_program, ParseError};
use aethershell::safety::SafetyError;
use aethershell::value::Value;

/// Evaluate `src` and return the value it produced.
fn eval(src: &str) -> Value {
    let program = parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, &mut env_with_modules())
        .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
}

/// The text of a source whose value is one string.
fn text(src: &str) -> String {
    match eval(src) {
        Value::Str(s) => s,
        other => panic!("expected a string, got {other:?}"),
    }
}

/// The failure evaluating `src` produces.
///
/// The source itself must parse: a hole's text is parsed while the literal is
/// evaluated, not while the program is.
fn refusal(src: &str) -> anyhow::Error {
    let program = parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, &mut env_with_modules())
        .expect_err(&format!("`{src}` must be refused"))
}

/// The `E_PARSE` a hole inside a string literal is refused with.
fn parse_error_of(src: &str) -> ParseError {
    let e = refusal(src);
    e.downcast_ref::<ParseError>()
        .cloned()
        .unwrap_or_else(|| panic!("expected a ParseError, got: {e}"))
}

/// The stable code of the structured failure `src` produces.
fn safety_code_of(src: &str) -> String {
    let e = refusal(src);
    e.downcast_ref::<SafetyError>()
        .map(|se| se.code.as_str().to_string())
        .unwrap_or_else(|| panic!("not a structured failure: {e}"))
}

// ── the interpolation that already worked ───────────────────────────────

#[test]
fn an_ordinary_hole_still_interpolates() {
    assert_eq!(text(r#""${1 + 2}""#), "3");
    assert_eq!(text(r#"let n = 7; "n=${n}""#), "n=7");
    assert_eq!(text(r#""a${'b'}c""#), "abc");
}

#[test]
fn a_hole_renders_records_and_arrays_the_way_str_does() {
    assert_eq!(text(r#""${{a: 1, b: [2, 3]}}""#), "{a: 1, b: [2, 3]}");
    assert_eq!(text(r#""v=${{a: {b: 1}}}""#), "v={a: {b: 1}}");
    assert_eq!(text(r#""${[1, 2, 3]}""#), "[1, 2, 3]");
}

#[test]
fn a_hole_can_hold_a_whole_expression_not_just_a_name() {
    assert_eq!(text(r#""${len([1, 2, 3])}""#), "3");
    assert_eq!(text(r#""${1 + 2 * 3}""#), "7");
}

#[test]
fn a_value_inserted_by_a_hole_is_not_interpolated_again() {
    // `inner` holds the text of a hole; inserting it must not run it.
    assert_eq!(text(r#"let inner = r"${1 + 1}"; "${inner}""#), "${1 + 1}");
}

// ── what the DSH session encoder generates ──────────────────────────────

#[test]
fn the_dollar_constant_the_session_encoder_writes_is_one_dollar() {
    // `encodeAetherStringLiteral` rewrites each `$` of a caller's text as
    // `${'$'}` inside the ordinary double-quoted literal it generates. That
    // interpolation must keep evaluating to exactly one `$`.
    assert_eq!(text(r#""${'$'}""#), "$");
}

#[test]
fn the_encoder_rewrite_leaves_the_callers_braces_alone() {
    // A caller's `${x}` becomes `"${'$'}{x}"`: one hole, then the caller's own
    // braces as text. Counting braces without quote awareness closed that hole
    // inside the single-quoted `'$'`.
    assert_eq!(text(r#""${'$'}{x}""#), "${x}");
}

#[test]
fn the_encoder_rewrite_survives_a_quote_and_braces_in_the_callers_text() {
    // The caller's text is `${"}"}`. Encoded, it is the source below, whose
    // decoded body is `${'$'}{"}"}` — and the hole must still end after `'$'`.
    assert_eq!(text(r#""${'$'}{\"}\"}""#), r#"${"}"}"#);
}

// ── quoted braces ───────────────────────────────────────────────────────

#[test]
fn a_brace_inside_a_single_quoted_string_does_not_close_the_hole() {
    assert_eq!(text(r#""${'}'}""#), "}");
}

#[test]
fn a_brace_inside_a_double_quoted_string_does_not_close_the_hole() {
    assert_eq!(text(r#""${\"}\" }""#), "}");
}

#[test]
fn an_escaped_quote_inside_a_hole_string_does_not_end_it() {
    // The quoted expression contains both an escaped quote and a literal brace.
    assert_eq!(text(r#""${\"a\\\"b}\"}""#), "a\"b}");
}

#[test]
fn a_raw_literal_in_a_hole_carries_its_braces() {
    // `r#"}"#` is the way to put a `}` in a hole: it is text, not a close.
    assert_eq!(text(r##""${r#\"}\"#}""##), "}");
}

#[test]
fn a_comment_in_a_hole_ignores_braces_until_the_line_ends() {
    assert_eq!(text("\"${1 # }\n}\""), "1");
    assert_eq!(text("\"${1 // }\n}\""), "1");
}

// ── text and holes around multi-byte characters ─────────────────────────

#[test]
fn interpolation_is_safe_next_to_multibyte_text() {
    assert_eq!(text("\"héllo ${'☃'} 你好\""), "héllo ☃ 你好");
    assert_eq!(text("\"☃${1}雪\""), "☃1雪");
}

// ── a hole that does not parse ──────────────────────────────────────────

#[test]
fn an_empty_hole_is_refused() {
    let pe = parse_error_of(r#""${}""#);
    assert!(pe.message.contains("empty"), "{}", pe.message);
    assert!(pe.message.contains("hole 1"), "{}", pe.message);
}

#[test]
fn a_hole_holding_a_statement_is_refused() {
    let pe = parse_error_of(r#""${let x = 1}""#);
    assert!(pe.message.contains("statement"), "{}", pe.message);
}

#[test]
fn a_hole_holding_two_statements_is_refused() {
    // The second statement used to vanish, so the literal answered `1`.
    let pe = parse_error_of(r#""${1; 2}""#);
    assert!(pe.message.contains("2 statements"), "{}", pe.message);
}

#[test]
fn a_hole_with_no_closing_brace_is_refused() {
    // The literal used to close silently and answer `answer3`.
    let pe = parse_error_of(r#""answer: ${1 + 2""#);
    assert!(pe.message.contains("no matching `}`"), "{}", pe.message);
    assert!(pe.message.contains("hole 1"), "{}", pe.message);
}

#[test]
fn an_unterminated_hole_names_which_hole_it_is() {
    let pe = parse_error_of(r#""first ${1} then ${1 + 2""#);
    assert!(pe.message.contains("hole 2"), "{}", pe.message);
}

#[test]
fn the_refusal_is_a_coded_parse_error() {
    // A hole whose own text does not parse is refused with the parser's own
    // diagnostics, which already carry `E_PARSE`.
    let e = refusal(r#""${1 +}""#);
    let text = e.to_string();
    assert!(
        text.contains(r#""code":"E_PARSE""#),
        "the hole's refusal is not coded: {text}"
    );
}

// ── a hole that fails while it runs ─────────────────────────────────────

#[test]
fn an_unknown_builtin_in_a_hole_keeps_its_code() {
    assert_eq!(
        safety_code_of(r#""${no_such_builtin_here()}""#),
        "E_UNKNOWN_BUILTIN"
    );
}

#[test]
fn a_division_by_zero_in_a_hole_keeps_its_code() {
    assert_eq!(safety_code_of(r#""${1/0}""#), "E_BAD_ARG");
}

// ── every hole is parsed before any hole runs ───────────────────────────

#[test]
fn a_malformed_hole_refuses_the_literal_before_an_earlier_hole_runs() {
    let dir = tempfile::tempdir().expect("private scratch directory");
    let path = dir.path().join("preflight.json");
    let bound = Value::Str(path.to_string_lossy().into_owned());

    // Positive control: the hole on its own does write the file, so a file
    // that is absent below is evidence and not a hole that never ran.
    let mut env = env_with_modules();
    env.set_var("fork_path", bound.clone())
        .expect("bind the path");
    let written = aethershell::eval::eval_program(
        &parse_program(r#""${save_json(fork_path, 1)}""#).expect("the literal parses"),
        &mut env,
    )
    .expect("a valid hole writes");
    assert!(matches!(written, Value::Str(_)), "{written:?}");
    assert!(path.exists(), "the positive control wrote no file");

    std::fs::remove_file(&path).expect("remove the control file");

    // The same hole, followed by one that does not parse.
    let mut env = env_with_modules();
    env.set_var("fork_path", bound).expect("bind the path");
    let e = aethershell::eval::eval_program(
        &parse_program(r#""${save_json(fork_path, 1)} ${1 +}""#).expect("the literal parses"),
        &mut env,
    )
    .expect_err("the second hole does not parse");
    assert!(e.downcast_ref::<ParseError>().is_some(), "{e}");
    assert!(
        !path.exists(),
        "a malformed hole let the hole before it run: {}",
        path.display()
    );
}

#[test]
fn every_hole_of_a_literal_is_evaluated_in_order() {
    // The preflight parses all of them; it must not skip the ones after the
    // first for a literal that is fine.
    assert_eq!(text(r#""${1}-${2}-${3}""#), "1-2-3");
}

// ── the Agent API eval envelope ─────────────────────────────────────────

/// The code inside the coded JSON text an `Eval` request answers with.
///
/// `execute_eval` puts the failure's own rendering in `error` (a string, not a
/// nested object), so the code is read back out of that text the way a caller
/// has to.
fn eval_error_code(code: &str) -> String {
    let response = process_request(&AgentRequest::Eval {
        code: code.to_string(),
    });
    assert!(
        !response.success,
        "`{code}` answered a successful envelope: {:?}",
        response.result
    );
    assert_eq!(
        response
            .metadata
            .as_ref()
            .and_then(|m| m.get("code_attempted"))
            .and_then(|c| c.as_str()),
        Some(code),
        "the envelope did not name the code it attempted"
    );
    let error = response.error.expect("a failed envelope carries `error`");
    let parsed: serde_json::Value =
        serde_json::from_str(&error).unwrap_or_else(|e| panic!("`{error}` is not coded JSON: {e}"));
    parsed["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no code in `{error}`"))
        .to_string()
}

#[test]
fn an_agent_api_eval_request_reports_a_hole_that_does_not_parse() {
    // The program itself parses — the failure is inside the literal.
    assert_eq!(eval_error_code(r#""answer ${1+2""#), "E_PARSE");
    assert_eq!(eval_error_code(r#""answer ${1+}""#), "E_PARSE");
}

#[test]
fn an_agent_api_eval_request_keeps_the_codes_a_hole_propagates() {
    assert_eq!(eval_error_code(r#""answer ${1/0}""#), "E_BAD_ARG");
    assert_eq!(
        eval_error_code(r#""answer ${no_such_builtin_here()}""#),
        "E_UNKNOWN_BUILTIN"
    );
}

#[test]
fn an_agent_api_eval_request_still_succeeds_for_a_valid_hole() {
    let response = process_request(&AgentRequest::Eval {
        code: r#""answer ${1 + 2}""#.to_string(),
    });
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(serde_json::json!("answer 3")));
}

//! JSON integer literals: the ones the native value space holds exactly, and
//! the ones that are refused instead of rounded.
//!
//! A JSON integer lexeme must fit the native 64-bit signed integer. `serde_json`
//! holds a `u64` exactly and derives an `f64` above that, so converting an
//! integer that fits neither rounds it: `json_parse("18446744073709551615")`
//! answered `18446744073709552000`, a different number that no caller could
//! detect from the answer. There is no exact value to return, so the parse is
//! refused with `E_BAD_ARG` and the literal is named in the message.
//!
//! The bound is on integer lexemes only. A `.`/`e`/`E` form is an `f64` by
//! definition and stays one, a number inside a quoted string or object key is
//! never a lexeme, and nothing here promises arbitrary precision: an id that
//! does not fit is meant to travel as a JSON string. Every assertion reaches
//! the builtins through `builtins::call`, `json.parse` module dispatch, a real
//! file, or an Agent API envelope — never through the validator — so an
//! implementation that only improved the wording could not pass.

use std::path::PathBuf;

use aethershell::agent_api::{process_request, AgentRequest, AgentResponse, PipelineStep};
use aethershell::builtins::call;
use aethershell::env::Env;
use aethershell::modules::env_with_modules;
use aethershell::safety::{current_mode, Mode};
use aethershell::value::Value;
use serde_json::{json, Value as JsonValue};

/// Integers at and beyond both `i64` edges, as the JSON text a caller sends.
const I64_MAX: &str = "9223372036854775807";
const ABOVE_I64_MAX: &str = "9223372036854775808";
const I64_MIN: &str = "-9223372036854775808";
const BELOW_I64_MIN: &str = "-9223372036854775809";
const U64_MAX: &str = "18446744073709551615";
const ABOVE_U64_MAX: &str = "18446744073709551616";

// ── calling the surfaces ────────────────────────────────────────────────

fn run(name: &str, args: Vec<Value>) -> Value {
    call(name, args, &mut Env::new()).unwrap_or_else(|e| panic!("{name} failed: {e}"))
}

/// The coded failure of a call that must not answer.
///
/// `Err` is the assertion: a `Float` answer, rounded or not, can never pass.
fn refusal(name: &str, args: Vec<Value>) -> JsonValue {
    match call(name, args, &mut Env::new()) {
        Ok(value) => panic!("{name} answered {value:?} instead of refusing"),
        Err(error) => coded(&error),
    }
}

/// A failure's own JSON document, which carries its code, message and hint.
fn coded(error: &anyhow::Error) -> JsonValue {
    let text = error.to_string();
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("`{text}` is not coded JSON"))
}

fn code(failure: &JsonValue) -> &str {
    failure["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no code in {failure}"))
}

fn message(failure: &JsonValue) -> &str {
    failure["error"]["message"].as_str().unwrap_or_default()
}

fn hint(failure: &JsonValue) -> &str {
    failure["error"]["hint"].as_str().unwrap_or_default()
}

/// The code a failed envelope carries, read from its `error` text — the form a
/// caller that keeps only the envelope has to read.
fn envelope_code(response: &AgentResponse) -> String {
    let text = response
        .error
        .clone()
        .unwrap_or_else(|| panic!("the response carries no error text"));
    let parsed: JsonValue =
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("`{text}` is not coded JSON"));
    parsed["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no code in `{text}`"))
        .to_string()
}

fn eval(src: &str) -> Value {
    let mut env = env_with_modules();
    let program =
        aethershell::parser::parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, &mut env)
        .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
}

fn eval_refusal(src: &str) -> JsonValue {
    let mut env = env_with_modules();
    let program =
        aethershell::parser::parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    match aethershell::eval::eval_program(&program, &mut env) {
        Ok(value) => panic!("`{src}` answered {value:?} instead of refusing"),
        Err(error) => coded(&error),
    }
}

fn text(value: &str) -> Value {
    Value::Str(value.to_string())
}

/// `value`'s field, panicking with the whole record when it is absent.
fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    match value {
        Value::Record(map) => map
            .get(name)
            .unwrap_or_else(|| panic!("no field `{name}` in {value:?}")),
        other => panic!("expected a Record, got {other:?}"),
    }
}

/// An integer lexeme of `digits` digits: far past `u64`, and still finite as an
/// `f64` while it stays under 309 digits.
fn long_integer(digits: usize) -> String {
    let mut literal = String::from("1");
    literal.push_str(&"0".repeat(digits - 1));
    literal
}

// ── a test's own directory ──────────────────────────────────────────────

/// The human profile these tests depend on — asserted, never set: mode is a
/// property of the process, and this binary runs its tests in parallel threads.
fn assert_human_mode() {
    assert_eq!(
        current_mode(),
        Mode::Human,
        "run these tests without AETHER_MODE, AETHER_AGENT or AETHER_WORKSPACE set"
    );
}

/// A private directory removed with the fixture, so a refusal can be shown to
/// leave nothing behind and no real user path is written to.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        assert_human_mode();
        Self {
            dir: tempfile::tempdir().expect("scratch directory"),
        }
    }

    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, content).expect("write the fixture");
        path
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path())
            .expect("readable scratch directory")
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

// ── the boundary ────────────────────────────────────────────────────────

#[test]
fn an_integer_the_native_value_space_holds_stays_exact() {
    // Both edges are inclusive: these are the largest and smallest integers an
    // Int can carry, and they must arrive unchanged.
    assert_eq!(run("json_parse", vec![text(I64_MAX)]), Value::Int(i64::MAX));
    assert_eq!(run("json_parse", vec![text(I64_MIN)]), Value::Int(i64::MIN));

    let document = format!(r#"{{"max": {I64_MAX}, "min": {I64_MIN}, "list": [{I64_MAX}, 0]}}"#);
    let parsed = run("json_parse", vec![text(&document)]);
    assert_eq!(field(&parsed, "max"), &Value::Int(i64::MAX));
    assert_eq!(field(&parsed, "min"), &Value::Int(i64::MIN));
    match field(&parsed, "list") {
        Value::Array(items) => assert_eq!(items.first(), Some(&Value::Int(i64::MAX))),
        other => panic!("`list` is not an Array: {other:?}"),
    }

    // And back out again through the same builtins: an exact integer does not
    // become text or a float on the way. `to_json` reads its value from the
    // pipe, so the round trip goes through the pipeline form.
    assert_eq!(run("from_json", vec![text(I64_MAX)]), Value::Int(i64::MAX));
    assert_eq!(
        aethershell::builtins::call_with_input(
            "to_json",
            vec![],
            Some(Value::Int(i64::MIN)),
            &mut Env::new(),
        )
        .unwrap_or_else(|e| panic!("to_json failed: {e}")),
        text(I64_MIN)
    );
}

#[test]
fn every_json_entry_point_applies_the_same_bound() {
    for name in ["json_parse", "from_json", "from-json"] {
        let failure = refusal(name, vec![text(U64_MAX)]);
        assert_eq!(code(&failure), "E_BAD_ARG", "{name}");
        assert!(
            message(&failure).contains(U64_MAX),
            "{name}: {}",
            message(&failure)
        );
        assert!(
            hint(&failure).contains("JSON strings"),
            "{name}: {}",
            hint(&failure)
        );
    }

    // Piped input and the `json.parse` module spelling read the same text.
    let piped = aethershell::builtins::call_with_input(
        "json_parse",
        vec![],
        Some(text(U64_MAX)),
        &mut Env::new(),
    )
    .expect_err("a piped large integer must be refused");
    assert_eq!(code(&coded(&piped)), "E_BAD_ARG");
    assert_eq!(
        code(&eval_refusal(&format!(r#"json.parse("{U64_MAX}")"#))),
        "E_BAD_ARG"
    );

    // The exact integer still arrives through the module spelling.
    assert_eq!(
        eval(&format!(r#"json.parse("{I64_MAX}")"#)),
        Value::Int(i64::MAX)
    );
}

#[test]
fn an_integer_outside_i64_is_refused_in_both_directions() {
    let literals = [
        ABOVE_I64_MAX,
        U64_MAX,
        ABOVE_U64_MAX,
        BELOW_I64_MIN,
        "-18446744073709551615",
        "-18446744073709551616",
    ];
    for literal in literals {
        let failure = refusal("json_parse", vec![text(literal)]);
        assert_eq!(code(&failure), "E_BAD_ARG", "{literal}: {failure}");
        assert!(
            message(&failure).contains(literal),
            "the refusal does not name the literal: {}",
            message(&failure)
        );
    }
}

#[test]
fn a_three_hundred_digit_integer_is_refused_in_both_signs() {
    let literal = long_integer(300);
    for candidate in [literal.clone(), format!("-{literal}")] {
        let failure = refusal("json_parse", vec![text(&candidate)]);
        assert_eq!(code(&failure), "E_BAD_ARG", "{candidate}");
        assert!(
            message(&failure).contains(&candidate),
            "{}",
            message(&failure)
        );
    }

    // A form that overflows even an `f64` stays the parser's own out-of-range
    // error — a 400-digit integer, or a non-finite exponent form — and the code
    // a caller branches on is the same one.
    for absurd in [long_integer(400), "1e400".to_string()] {
        assert_eq!(
            code(&refusal("json_parse", vec![text(&absurd)])),
            "E_BAD_ARG"
        );
    }
}

#[test]
fn a_decimal_or_exponent_lexeme_stays_a_float() {
    // Judged by the lexeme, not by its size: these carry `.` or `e`, so they
    // were always an `f64` and remain one.
    assert_eq!(run("json_parse", vec![text("1.5")]), Value::Float(1.5));
    assert_eq!(run("json_parse", vec![text("2e3")]), Value::Float(2000.0));
    assert_eq!(run("json_parse", vec![text("2E3")]), Value::Float(2000.0));
    assert_eq!(
        run("json_parse", vec![text("-1.5e-3")]),
        Value::Float(-0.0015)
    );
    // A decimal form too large for an integer is still a Float, because it was
    // never an integer lexeme.
    assert_eq!(
        run("json_parse", vec![text("1.8446744073709552e19")]),
        Value::Float(1.8446744073709552e19)
    );
    assert!(matches!(
        run("json_parse", vec![text("1e30")]),
        Value::Float(_)
    ));
}

#[test]
fn negative_zero_keeps_the_form_the_parser_already_chose() {
    // `-0` is an integer lexeme inside the range, so the bound has no say in
    // it: what the parser already produced is preserved, not re-decided here.
    let parsed = run("json_parse", vec![text("-0")]);
    match parsed {
        // `-0.0 == 0.0` in IEEE 754, so the sign bit is the assertion.
        Value::Float(value) => {
            assert!(
                value == 0.0 && value.is_sign_negative(),
                "{value} lost its sign"
            )
        }
        other => panic!("`-0` changed form: {other:?}"),
    }
    assert_eq!(run("json_parse", vec![text("0")]), Value::Int(0));
}

#[test]
fn a_number_inside_a_string_or_a_key_is_never_a_lexeme() {
    // The exact ids an integer cannot hold still travel as text, at the top
    // level, nested, and as an object key.
    let document = format!(
        r#"{{"id": "{U64_MAX}", "next": "{ABOVE_U64_MAX}", "nested": {{"k": "{BELOW_I64_MIN}"}}, "{I64_MAX}": 1}}"#
    );
    let parsed = run("json_parse", vec![text(&document)]);
    assert_eq!(field(&parsed, "id"), &text(U64_MAX));
    assert_eq!(field(&parsed, "next"), &text(ABOVE_U64_MAX));
    assert_ne!(field(&parsed, "id"), field(&parsed, "next"));
    assert_eq!(field(field(&parsed, "nested"), "k"), &text(BELOW_I64_MIN));
    assert_eq!(field(&parsed, I64_MAX), &Value::Int(1));

    // An escaped quote and a backslash do not end the string, and a non-ASCII
    // prefix does not shift what is read: the digits after them are inside it.
    let escaped = r#"{"s": "he said \"18446744073709551615\" and \\ 99999999999999999999", "u": "名前 18446744073709551615"}"#;
    let parsed = run("json_parse", vec![text(escaped)]);
    assert_eq!(
        field(&parsed, "s"),
        &text(r#"he said "18446744073709551615" and \ 99999999999999999999"#)
    );
    assert_eq!(field(&parsed, "u"), &text("名前 18446744073709551615"));
}

#[test]
fn a_nested_out_of_range_integer_refuses_the_whole_document() {
    // The parse is refused, not the field: no partial value escapes with the
    // large integer quietly rounded inside it.
    let nested = format!(r#"{{"rows": [{{"id": 1, "big": {U64_MAX}}}]}}"#);
    assert_eq!(
        code(&refusal("json_parse", vec![text(&nested)])),
        "E_BAD_ARG"
    );

    // Even a branch nobody reads is part of the document.
    let elsewhere = format!(r#"{{"rows": [], "meta": {{"count": {ABOVE_I64_MAX}}}}}"#);
    assert_eq!(
        code(&refusal("from_json", vec![text(&elsewhere)])),
        "E_BAD_ARG"
    );
}

// ── files ───────────────────────────────────────────────────────────────

#[test]
fn a_json_file_reads_exactly_and_refuses_the_out_of_range_literal() {
    let fixture = Fixture::new();
    let exact = fixture.write(
        "ids.json",
        &format!(r#"{{"max": {I64_MAX}, "min": {I64_MIN}, "id": "{U64_MAX}"}}"#),
    );
    let read = run("open", vec![text(&exact.to_string_lossy())]);
    assert_eq!(field(&read, "max"), &Value::Int(i64::MAX));
    assert_eq!(field(&read, "min"), &Value::Int(i64::MIN));
    assert_eq!(field(&read, "id"), &text(U64_MAX));

    let out_of_range = fixture.write("big.json", &format!(r#"{{"id": {U64_MAX}}}"#));
    let before = std::fs::read(&out_of_range).expect("external read");
    let failure = refusal("open", vec![text(&out_of_range.to_string_lossy())]);
    assert_eq!(code(&failure), "E_BAD_ARG");
    assert!(message(&failure).contains(U64_MAX), "{}", message(&failure));

    // A refusal reads the file and changes nothing: same bytes, same entries,
    // no temporary file left behind.
    assert_eq!(std::fs::read(&out_of_range).expect("external read"), before);
    assert_eq!(
        fixture.entries(),
        vec!["big.json".to_string(), "ids.json".to_string()]
    );
}

#[test]
fn a_jsonl_line_with_an_out_of_range_integer_refuses_the_file() {
    let fixture = Fixture::new();
    let valid = fixture.write(
        "rows.jsonl",
        &format!("{{\"id\": {I64_MAX}}}\n{{\"id\": {I64_MIN}}}\n"),
    );
    let rows = run("open", vec![text(&valid.to_string_lossy())]);
    let rows = match rows {
        Value::Array(rows) => rows,
        other => panic!("expected an Array, got {other:?}"),
    };
    assert_eq!(rows.len(), 2, "a JSONL file lost a row: {rows:?}");
    assert_eq!(field(&rows[0], "id"), &Value::Int(i64::MAX));
    assert_eq!(field(&rows[1], "id"), &Value::Int(i64::MIN));

    // One row with an out-of-range integer refuses the file, not just that row.
    let mixed = fixture.write(
        "mixed.jsonl",
        &format!("{{\"id\": 1}}\n{{\"id\": {U64_MAX}}}\n{{\"id\": 2}}\n"),
    );
    let before = std::fs::read(&mixed).expect("external read");
    let failure = refusal("open", vec![text(&mixed.to_string_lossy())]);
    assert_eq!(code(&failure), "E_BAD_ARG");
    assert!(message(&failure).contains(U64_MAX), "{}", message(&failure));
    assert_eq!(std::fs::read(&mixed).expect("external read"), before);
    assert_eq!(
        fixture.entries(),
        vec!["mixed.jsonl".to_string(), "rows.jsonl".to_string()]
    );
}

// ── the Agent API envelopes ─────────────────────────────────────────────

#[test]
fn the_agent_api_envelopes_report_the_refusal() {
    let evaluated = process_request(&AgentRequest::Eval {
        code: format!(r#"json_parse("{U64_MAX}")"#),
    });
    assert!(!evaluated.success, "answered: {:?}", evaluated.result);
    assert_eq!(envelope_code(&evaluated), "E_BAD_ARG");

    let called = process_request(&AgentRequest::Call {
        builtin: "json_parse".to_string(),
        args: json!([U64_MAX]),
    });
    assert!(!called.success, "answered: {:?}", called.result);
    assert_eq!(envelope_code(&called), "E_BAD_ARG");
    assert_eq!(
        called
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("code"))
            .and_then(|code| code.as_str()),
        Some("E_BAD_ARG"),
        "the envelope does not repeat the code in metadata: {:?}",
        called.metadata
    );

    // A pipeline step is refused the same way.
    let piped = process_request(&AgentRequest::Pipeline {
        steps: vec![PipelineStep {
            builtin: "json_parse".to_string(),
            args: json!([U64_MAX]),
            select: None,
            predicate: None,
        }],
        input: None,
    });
    assert!(!piped.success, "answered: {:?}", piped.result);
    assert_eq!(envelope_code(&piped), "E_BAD_ARG");

    // Every argument shape the typed API accepted still reaches the builtin.
    for args in [
        json!([I64_MAX]),
        json!(I64_MAX),
        json!({ "input": I64_MAX }),
    ] {
        let accepted = process_request(&AgentRequest::Call {
            builtin: "json_parse".to_string(),
            args,
        });
        assert!(accepted.success, "failed: {:?}", accepted.error);
        assert_eq!(accepted.result, Some(json!(i64::MAX)));
    }
}

//! `metadata.error`: a failure's own data, readable without re-parsing `error`.
//!
//! A failed envelope stated its coded failure only inside the `error` string, so
//! a caller had to parse JSON out of a JSON field to learn the code, the
//! expected shape, or whether a retry could help. Every failure now also states
//! that data as an object at `metadata.error`, together with the producer's own
//! facts: which phase failed (`arguments`, `preparation`, `parse`, `execution`,
//! `evaluation`), whether anything could have run (`side_effects`), and, for a
//! pipeline, the zero-based step that failed plus the builtin dispatches that
//! completed before it.
//!
//! `retryable` keeps the meaning the structured text always gave it — a
//! correction to the call can plausibly succeed. `safe_to_retry` is the
//! narrower claim that re-issuing this exchange repeats nothing, and it is
//! `false` everywhere: a failure reported after a call may have applied part of
//! that call's effect, and nothing here promises a replay is free of it.
//!
//! The legacy `error` string is unchanged, and a *successful* result is never
//! inspected for failure data: a value that happens to contain an error document
//! is business data, not a failure.

#![cfg(feature = "native")]

use aethershell::agent_api::{
    process_json_request, process_request, AgentRequest, AgentResponse, PipelineStep,
};
use serde_json::{json, Map, Value as JsonValue};

// ── request helpers ──────────────────────────────────────────────────────

fn call(builtin: &str, args: JsonValue) -> AgentResponse {
    process_request(&AgentRequest::Call {
        builtin: builtin.to_string(),
        args,
    })
}

fn eval(code: &str) -> AgentResponse {
    process_request(&AgentRequest::Eval {
        code: code.to_string(),
    })
}

fn pipeline(steps: Vec<PipelineStep>, input: Option<JsonValue>) -> AgentResponse {
    process_request(&AgentRequest::Pipeline { steps, input })
}

fn step(builtin: &str, args: JsonValue) -> PipelineStep {
    PipelineStep {
        builtin: builtin.to_string(),
        args,
        select: None,
        predicate: None,
    }
}

/// A step that really writes, so "the step before this one ran" is a claim
/// about the host rather than a claim about the envelope.
fn write_step(path: &std::path::Path) -> PipelineStep {
    step(
        "save_json",
        json!([path.to_string_lossy(), { "written": true }]),
    )
}

// ── failure readers ──────────────────────────────────────────────────────

/// The `metadata.error` object.
///
/// A string here would mean the failure data was encoded a second time, which
/// is the defect these tests exist to catch.
fn error_of(response: &AgentResponse) -> &Map<String, JsonValue> {
    let metadata = response
        .metadata
        .as_ref()
        .unwrap_or_else(|| panic!("the failure carries no metadata: {:?}", response.error));
    let error = metadata
        .get("error")
        .unwrap_or_else(|| panic!("the failure carries no metadata.error: {metadata}"));
    assert!(
        error.is_object(),
        "metadata.error is not directly readable: {error}"
    );
    error.as_object().expect("checked just above")
}

/// One field of `metadata.error`.
fn field<'a>(response: &'a AgentResponse, key: &str) -> &'a JsonValue {
    error_of(response)
        .get(key)
        .unwrap_or_else(|| panic!("metadata.error names no `{key}`: {:?}", error_of(response)))
}

/// A string field of `metadata.error`.
fn text_of(response: &AgentResponse, key: &str) -> String {
    field(response, key)
        .as_str()
        .unwrap_or_else(|| panic!("`{key}` is not a string: {:?}", field(response, key)))
        .to_string()
}

/// The stable code of a failed response.
fn code_of(response: &AgentResponse) -> String {
    text_of(response, "code")
}

/// The code a pre-existing caller reads out of the legacy `error` string.
fn legacy_code(response: &AgentResponse) -> String {
    let text = response
        .error
        .clone()
        .expect("a failure carries `error` text");
    let parsed: JsonValue =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("`{text}` is not coded JSON: {e}"));
    parsed["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no code in `{text}`"))
        .to_string()
}

// ── a typed call: an argument refused before dispatch ────────────────────

#[test]
fn a_typed_argument_failure_is_readable_from_metadata_error() {
    // A JSON integer above `i64::MAX` has no exact native form, so the request
    // is refused while its arguments are converted.
    let response = call("type_of", json!([u64::MAX]));
    assert!(!response.success, "answered: {:?}", response.result);

    // The object, not a re-encoded string: each field keeps its own JSON type.
    assert_eq!(field(&response, "code"), &json!("E_BAD_ARG"));
    assert!(field(&response, "message").is_string());
    assert!(field(&response, "hint").is_string());
    assert!(field(&response, "builtin").is_string());
    assert!(field(&response, "expected").is_string());
    assert!(field(&response, "got").is_string());
    assert!(field(&response, "retryable").is_boolean());
    assert!(field(&response, "safe_to_retry").is_boolean());
    assert!(
        field(&response, "builtin").as_str() == Some("type_of"),
        "the failing builtin is not named: {:?}",
        error_of(&response)
    );

    // The facts this refusal's site knows, and no others.
    assert_eq!(text_of(&response, "phase"), "arguments");
    assert_eq!(text_of(&response, "side_effects"), "none");
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));
    assert_eq!(field(&response, "retryable"), &json!(true));
    assert!(
        !error_of(&response).contains_key("step_index"),
        "a call has no pipeline step: {:?}",
        error_of(&response)
    );
}

#[test]
fn the_legacy_error_string_still_parses_to_the_same_code() {
    let response = call("type_of", json!([u64::MAX]));
    assert!(
        response.error.is_some(),
        "the legacy string was dropped for the object"
    );
    assert_eq!(legacy_code(&response), code_of(&response));
    assert_eq!(legacy_code(&response), "E_BAD_ARG");
}

#[test]
fn an_unsafe_integer_is_named_and_never_rounded() {
    // A JS-unsafe integer arrives as JSON text; the refusal names it exactly
    // rather than answering a rounded number.
    let text = process_json_request(
        r#"{"action":"call","builtin":"type_of","args":[18446744073709551615]}"#,
    )
    .expect("the request deserializes");
    let response: AgentResponse = serde_json::from_str(&text).expect("the reply is an envelope");
    assert!(!response.success, "answered: {:?}", response.result);
    assert_eq!(code_of(&response), "E_BAD_ARG");
    assert!(
        text_of(&response, "got").contains("18446744073709551615"),
        "the refused integer is not named: {:?}",
        error_of(&response)
    );
    assert!(
        !text_of(&response, "got").contains("18446744073709552000"),
        "the integer was silently rounded: {:?}",
        error_of(&response)
    );

    // The other side of the same bound: every integer the native value space
    // holds exactly still crosses the typed API unchanged.
    assert_eq!(
        call("json_stringify", json!([i64::MAX])).result,
        Some(json!("9223372036854775807"))
    );
    assert_eq!(
        call("json_stringify", json!([i64::MIN])).result,
        Some(json!("-9223372036854775808"))
    );
}

// ── a builtin that failed after dispatch ─────────────────────────────────

#[test]
fn a_builtin_runtime_failure_claims_unknown_side_effects() {
    // `json_parse` accepts the string and refuses its integer lexeme itself, so
    // this failure comes from dispatch rather than from argument conversion.
    let response = call("json_parse", json!(["18446744073709551615"]));
    assert!(!response.success);
    assert_eq!(code_of(&response), "E_BAD_ARG");
    assert_eq!(text_of(&response, "phase"), "execution");
    assert_eq!(text_of(&response, "side_effects"), "unknown");
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));
    assert!(
        text_of(&response, "got").contains("18446744073709551615"),
        "the refused lexeme is not named: {:?}",
        error_of(&response)
    );
}

#[test]
fn retryable_keeps_its_meaning_while_safe_to_retry_stays_false() {
    // Retryable: a corrected call can succeed.
    let unknown = call("no_such_builtin_9f3a", json!([]));
    assert_eq!(code_of(&unknown), "E_UNKNOWN_BUILTIN");
    assert_eq!(field(&unknown, "retryable"), &json!(true));
    assert_eq!(field(&unknown, "safe_to_retry"), &json!(false));

    // Not retryable: the taxonomy's own `E_UNKNOWN`, whose contract says stop
    // rather than re-run. `safe_to_retry` is still the same `false`.
    let uncoded = call("len", json!([1]));
    assert_eq!(code_of(&uncoded), "E_UNKNOWN");
    assert_eq!(field(&uncoded, "retryable"), &json!(false));
    assert_eq!(field(&uncoded, "safe_to_retry"), &json!(false));
}

// ── eval: parse versus evaluation ────────────────────────────────────────

#[test]
fn an_initial_parse_failure_is_phase_parse_with_no_effects() {
    let response = eval("let x = ");
    assert!(!response.success);
    assert_eq!(code_of(&response), "E_PARSE");
    assert_eq!(text_of(&response, "phase"), "parse");
    assert_eq!(text_of(&response, "side_effects"), "none");
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));
    assert!(
        text_of(&response, "message").contains("unexpected token"),
        "the parser's diagnostic is missing: {:?}",
        error_of(&response)
    );
    assert!(
        text_of(&response, "hint").contains("nothing ran"),
        "the no-effects guarantee is missing: {:?}",
        error_of(&response)
    );
    // The program is still reported exactly as attempted.
    assert_eq!(field(&response, "code"), &json!("E_PARSE"));
    assert_eq!(
        response
            .metadata
            .as_ref()
            .and_then(|m| m.get("code_attempted")),
        Some(&json!("let x = "))
    );
}

#[test]
fn an_interpolation_parse_failure_after_a_statement_claims_unknown_effects() {
    let dir = tempfile::tempdir().expect("private scratch directory");
    let marker = dir.path().join("ran.json");
    // The path travels inside source, so backslashes are doubled by the lexer's
    // escape rules; forward slashes are the same path on this platform.
    let path = marker.to_string_lossy().replace('\\', "/");
    let code = format!("save_json(\"{path}\", 1)\n\"${{}}\"");

    let response = eval(&code);
    assert!(!response.success, "answered: {:?}", response.result);

    // The code is `E_PARSE` — the hole is parsed while its literal is evaluated
    // — but that alone cannot say nothing ran: the statement before it did.
    assert_eq!(code_of(&response), "E_PARSE");
    assert_eq!(text_of(&response, "phase"), "evaluation");
    assert_eq!(text_of(&response, "side_effects"), "unknown");
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));
    assert!(
        text_of(&response, "hint").contains("nothing ran"),
        "the hole's own hint changed: {:?}",
        error_of(&response)
    );
    assert!(
        marker.exists(),
        "the earlier statement never ran, so the unknown claim is untested"
    );
}

// ── pipelines: preparation versus dispatch ───────────────────────────────

#[test]
fn a_malformed_later_step_is_preparation_with_zero_completed_steps() {
    let dir = tempfile::tempdir().expect("private scratch directory");
    let marker = dir.path().join("prepared.json");
    let mut bad_select = step("json_parse", json!(["[1]"]));
    bad_select.select = Some("a..b".to_string());

    let response = pipeline(vec![write_step(&marker), bad_select], None);
    assert!(!response.success);
    assert_eq!(code_of(&response), "E_BAD_ARG");
    assert_eq!(text_of(&response, "phase"), "preparation");
    assert_eq!(text_of(&response, "side_effects"), "none");
    assert_eq!(field(&response, "step_index"), &json!(1));
    assert_eq!(field(&response, "completed_steps"), &json!(0));
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));
    assert!(
        !marker.exists(),
        "a description refused in preparation ran the step before it"
    );
}

#[test]
fn an_empty_pipeline_states_preparation_and_no_step_facts() {
    let response = pipeline(vec![], None);
    assert!(!response.success);
    assert_eq!(code_of(&response), "E_BAD_ARG");
    assert_eq!(text_of(&response, "phase"), "preparation");
    assert_eq!(text_of(&response, "side_effects"), "none");
    assert!(
        !error_of(&response).contains_key("step_index"),
        "an empty pipeline has no failing step: {:?}",
        error_of(&response)
    );
}

#[test]
fn a_runtime_failure_names_its_step_and_the_dispatches_before_it() {
    let dir = tempfile::tempdir().expect("private scratch directory");
    let marker = dir.path().join("ran.json");

    let response = pipeline(
        vec![write_step(&marker), step("no_such_builtin_9f3a", json!([]))],
        None,
    );
    assert!(!response.success);
    assert_eq!(code_of(&response), "E_UNKNOWN_BUILTIN");
    assert_eq!(text_of(&response, "phase"), "execution");
    assert_eq!(text_of(&response, "side_effects"), "unknown");
    assert_eq!(field(&response, "step_index"), &json!(1));
    assert_eq!(field(&response, "completed_steps"), &json!(1));
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));
    assert!(
        marker.exists(),
        "the counted step did not run, so `completed_steps` is wrong"
    );
}

#[test]
fn a_select_failure_counts_the_builtin_that_completed() {
    // The one dispatch succeeded; the selection over its result is what failed.
    let mut selected = step("json_parse", json!([r#"{"a":1}"#]));
    selected.select = Some("a".to_string());

    let response = pipeline(vec![selected], None);
    assert!(!response.success);
    assert_eq!(code_of(&response), "E_BAD_ARG");
    assert_eq!(text_of(&response, "phase"), "execution");
    assert_eq!(text_of(&response, "side_effects"), "unknown");
    assert_eq!(field(&response, "step_index"), &json!(0));
    assert_eq!(field(&response, "completed_steps"), &json!(1));
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));
}

#[test]
fn a_second_annotation_pass_keeps_the_facts_a_producer_stated() {
    // `error_response` writes `metadata.error` and `process_request` annotates
    // the same envelope again. The fail-closed defaults that second pass fills
    // must never replace what the refusing site already knew, so the
    // non-default `side_effects: "none"` and the phase survive both passes.
    let response = eval("let x = ");
    assert_eq!(text_of(&response, "phase"), "parse");
    assert_eq!(text_of(&response, "side_effects"), "none");
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));

    // A step's position facts survive the same second pass, and the annotation
    // owns exactly one `error` key either way.
    let mut bad_select = step("json_parse", json!(["[1]"]));
    bad_select.select = Some("a..b".to_string());
    let pipelined = pipeline(vec![bad_select], None);
    assert_eq!(text_of(&pipelined, "phase"), "preparation");
    assert_eq!(text_of(&pipelined, "side_effects"), "none");
    assert_eq!(field(&pipelined, "step_index"), &json!(0));
    assert_eq!(field(&pipelined, "completed_steps"), &json!(0));
    assert_eq!(
        pipelined
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.as_object())
            .map(|metadata| metadata.keys().filter(|key| *key == "error").count()),
        Some(1)
    );
}

// ── the fallback and the success case ────────────────────────────────────

#[test]
fn a_failure_without_a_code_falls_back_to_unknown_and_invents_nothing() {
    let response = process_request(&AgentRequest::Describe {
        builtin: "no_such_builtin_9f3a".to_string(),
    });
    assert!(!response.success);
    assert_eq!(code_of(&response), "E_UNKNOWN");
    assert_eq!(
        text_of(&response, "message"),
        response.error.clone().unwrap_or_default(),
        "the message is not the failure's own text"
    );
    assert_eq!(text_of(&response, "side_effects"), "unknown");
    assert_eq!(field(&response, "safe_to_retry"), &json!(false));
    assert!(
        !error_of(&response).contains_key("retryable"),
        "a retry claim was invented: {:?}",
        error_of(&response)
    );
}

#[test]
fn a_successful_result_that_looks_like_an_error_is_left_alone() {
    // The business value *is* an error document. Nothing about it is a failure.
    let response = call(
        "json_parse",
        json!([r#"{"error":{"code":"E_FAKE","message":"business data"}}"#]),
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(
        response.result,
        Some(json!({"error": {"code": "E_FAKE", "message": "business data"}}))
    );
    assert!(
        response.error.is_none(),
        "a success gained an error string: {:?}",
        response.error
    );
    let metadata = response.metadata.as_ref().expect("metadata present");
    assert!(
        metadata.get("error").is_none(),
        "a success gained a failure object: {metadata}"
    );
    assert!(
        metadata.get("token_accounting").is_some(),
        "accounting is missing: {metadata}"
    );

    // A business record that itself claims failure is still just a value.
    let record = eval(r#"json_parse("{\"success\":false,\"error\":\"boom\"}")"#);
    assert!(record.success, "failed: {:?}", record.error);
    assert_eq!(
        record.result,
        Some(json!({"success": false, "error": "boom"}))
    );
    assert!(
        record
            .metadata
            .as_ref()
            .is_some_and(|m| m.get("error").is_none()),
        "a success gained a failure object: {:?}",
        record.metadata
    );

    // The same value as eval text: a successful program answers its result, and
    // the envelope still says nothing about a failure.
    let text = process_json_request(
        r#"{"action":"eval","code":"json_parse(\"{\\\"error\\\":{\\\"code\\\":\\\"E_FAKE\\\"}}\")"}"#,
    )
    .expect("the request deserializes");
    let evaluated: AgentResponse = serde_json::from_str(&text).expect("the reply is an envelope");
    assert!(evaluated.success, "failed: {:?}", evaluated.error);
    assert!(
        evaluated
            .metadata
            .as_ref()
            .is_some_and(|m| m.get("error").is_none()),
        "a success gained a failure object: {:?}",
        evaluated.metadata
    );
}

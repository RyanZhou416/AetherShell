//! The typed Agent API: a request's JSON values reach the interpreter as data.
//!
//! The API used to render every argument into AetherShell source and evaluate
//! that string. Two conversions made otherwise inert data executable — a string
//! containing `${` was interpolated as an expression, and an object key was
//! emitted unquoted — and a top-level object naming `input` or `value` had every
//! other field silently dropped.
//!
//! `call` and `pipeline` now convert JSON straight to native values and dispatch
//! through the one builtin entry point, so effect policy, signature validation
//! and the safety gate are unchanged while the value channel stays inert.
//! `eval` is still the deliberate source escape hatch.
//!
//! These tests pin the contract an agent depends on: what shape each request
//! field accepts, what the envelope says about how it was served, and that no
//! request value is ever re-read as source. Every assertion is on the value
//! space, so a result that merely prints the right text cannot pass.

use aethershell::agent_api::{
    process_json_request, process_request, AgentRequest, AgentResponse, PipelineStep,
};
use serde_json::{json, Value as JsonValue};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// ── Request helpers ──────────────────────────────────────────────────────

fn call(builtin: &str, args: JsonValue) -> AgentResponse {
    process_request(&AgentRequest::Call {
        builtin: builtin.to_string(),
        args,
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

/// The structured failure an envelope carries, as a JSON document.
///
/// A coded failure renders as `{"error":{…}}`; anything else means the failure
/// lost its code on the way out, which is itself the defect these tests exist
/// to catch.
fn failure(response: &AgentResponse) -> JsonValue {
    let text = response
        .error
        .clone()
        .unwrap_or_else(|| panic!("the response carries no error text"));
    let parsed: JsonValue = serde_json::from_str(&text)
        .unwrap_or_else(|_| panic!("the failure is not the structured form: {text}"));
    assert!(
        parsed.get("error").is_some_and(|e| e.is_object()),
        "the failure names no code: {text}"
    );
    parsed
}

/// The stable code of a failed response.
fn code(response: &AgentResponse) -> String {
    failure(response)["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn metadata(response: &AgentResponse) -> &JsonValue {
    response.metadata.as_ref().expect("metadata present")
}

/// A private temporary directory, removed when it goes out of scope.
///
/// The name carries a wall-clock nonce plus a per-run counter, so a re-run or a
/// second test in this file cannot land on the same directory — and no real
/// user path is written to.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "ae_fork_typed_{tag}_{nanos:x}_{}",
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

// ── The capability probe ─────────────────────────────────────────────────

#[test]
fn capabilities_report_exactly_the_frozen_protocol() {
    let response = process_json_request(r#"{"action":"capabilities"}"#)
        .expect("a capabilities probe deserializes");
    let parsed: AgentResponse = serde_json::from_str(&response).expect("the reply is an envelope");

    assert!(parsed.success, "the probe failed: {:?}", parsed.error);
    assert_eq!(parsed.result_type.as_deref(), Some("Capabilities"));
    // Exact: a caller turns on a typed behavior only when the flag it names is
    // present and true, so a missing or renamed flag must fail here.
    assert_eq!(
        parsed.result,
        Some(json!({
            "protocol": "aethershell-agent-api",
            "protocol_version": 1,
            "features": {
                "typed_arguments_v1": true,
                "typed_pipeline_input_v1": true,
                "explicit_null_pipeline_input_v1": true,
            },
        }))
    );
    // The probe is not a special envelope: it still reports its own cost.
    assert!(
        metadata(&parsed).get("token_accounting").is_some(),
        "the probe carries no accounting: {}",
        metadata(&parsed)
    );
}

#[test]
fn the_capability_probe_is_pure_and_serializes_as_the_action_it_names() {
    assert_eq!(
        serde_json::to_string(&AgentRequest::Capabilities).expect("serializes"),
        r#"{"action":"capabilities"}"#
    );

    // No observable state between two probes: the same question has the same
    // answer, and nothing about the second depends on the first.
    let first = process_request(&AgentRequest::Capabilities);
    let second = process_request(&AgentRequest::Capabilities);
    assert_eq!(first.result, second.result);
}

// ── The pipeline seed: absent versus present-null ────────────────────────

#[test]
fn serde_distinguishes_an_absent_seed_from_a_present_null() {
    let absent: AgentRequest = serde_json::from_str(r#"{"action":"pipeline","steps":[]}"#)
        .expect("an omitted input deserializes");
    let AgentRequest::Pipeline { input, .. } = absent else {
        panic!("not a pipeline request");
    };
    assert_eq!(input, None, "an absent input must stay None");

    let present: AgentRequest =
        serde_json::from_str(r#"{"action":"pipeline","steps":[],"input":null}"#)
            .expect("an explicit null deserializes");
    let AgentRequest::Pipeline { input, .. } = present else {
        panic!("not a pipeline request");
    };
    assert_eq!(
        input,
        Some(JsonValue::Null),
        "an explicit null must be the Null seed, not the absent one"
    );

    let falsey: AgentRequest =
        serde_json::from_str(r#"{"action":"pipeline","steps":[],"input":false}"#)
            .expect("a false seed deserializes");
    let AgentRequest::Pipeline { input, .. } = falsey else {
        panic!("not a pipeline request");
    };
    assert_eq!(input, Some(json!(false)));
}

#[test]
fn an_omitted_seed_serializes_as_omitted_and_does_not_come_back_as_null() {
    let omitted = AgentRequest::Pipeline {
        steps: vec![],
        input: None,
    };
    let text = serde_json::to_string(&omitted).expect("serializes");
    assert!(
        !text.contains("input"),
        "an absent seed must not be written as null: {text}"
    );
    let reparsed: AgentRequest = serde_json::from_str(&text).expect("round trips");
    let AgentRequest::Pipeline { input, .. } = reparsed else {
        panic!("not a pipeline request");
    };
    assert_eq!(input, None, "the absent seed changed across a round trip");

    let explicit = AgentRequest::Pipeline {
        steps: vec![],
        input: Some(JsonValue::Null),
    };
    let text = serde_json::to_string(&explicit).expect("serializes");
    assert!(
        text.contains(r#""input":null"#),
        "an explicit null seed must survive serialization: {text}"
    );
    let reparsed: AgentRequest = serde_json::from_str(&text).expect("round trips");
    let AgentRequest::Pipeline { input, .. } = reparsed else {
        panic!("not a pipeline request");
    };
    assert_eq!(
        input,
        Some(JsonValue::Null),
        "the explicit null seed changed across a round trip"
    );
}

#[test]
fn an_explicit_null_seed_reaches_the_first_step_as_the_null_value() {
    let response = pipeline(vec![step("type_of", json!(null))], Some(json!(null)));
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!("Null")));
    assert_eq!(response.result_type.as_deref(), Some("String"));

    // The same request spelled as JSON, which is how a caller actually sends it.
    let text = process_json_request(
        r#"{"action":"pipeline","input":null,"steps":[{"builtin":"type_of"}]}"#,
    )
    .expect("deserializes");
    let parsed: AgentResponse = serde_json::from_str(&text).expect("envelope");
    assert!(parsed.success, "failed: {:?}", parsed.error);
    assert_eq!(parsed.result, Some(json!("Null")));

    // And the contrast: with no seed at all, the step has nothing to read.
    let without = pipeline(vec![step("type_of", json!(null))], None);
    assert!(
        !without.success,
        "type_of with no value at all must fail, not answer"
    );
    assert_eq!(code(&without), "E_BAD_ARG");
}

// ── `args`: positional array, scalars, and the legacy named form ─────────

#[test]
fn an_omitted_or_null_args_field_means_no_arguments() {
    for request in [
        r#"{"action":"call","builtin":"type_of"}"#,
        r#"{"action":"call","builtin":"type_of","args":null}"#,
        r#"{"action":"call","builtin":"type_of","args":[]}"#,
        r#"{"action":"call","builtin":"type_of","args":{}}"#,
    ] {
        let text = process_json_request(request).unwrap_or_else(|e| panic!("{request}: {e}"));
        let parsed: AgentResponse = serde_json::from_str(&text).expect("envelope");
        assert!(
            !parsed.success,
            "`{request}` must be a call with no arguments, which type_of refuses"
        );
        assert_eq!(code(&parsed), "E_BAD_ARG", "`{request}`");
    }
}

#[test]
fn a_null_inside_the_argument_array_is_one_null_argument() {
    let response = call("type_of", json!([null]));
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!("Null")));

    // The distinction the two spellings carry, side by side.
    let none = call("type_of", json!(null));
    assert!(!none.success, "no arguments is not the same request");
}

#[test]
fn a_scalar_argument_is_one_positional_argument() {
    assert_eq!(call("type_of", json!(42)).result, Some(json!("Int")));
    assert_eq!(call("type_of", json!(1.5)).result, Some(json!("Float")));
    assert_eq!(call("type_of", json!(true)).result, Some(json!("Bool")));
    assert_eq!(call("len", json!("hello")).result, Some(json!(5)));
}

#[test]
fn the_legacy_named_object_is_still_the_argument_it_names() {
    // The documented pre-typed forms, and the ones an older SDK still sends.
    for key in ["input", "value", "path"] {
        let mut args = serde_json::Map::new();
        args.insert(key.to_string(), json!("hello"));
        let response = call("len", JsonValue::Object(args));
        assert!(
            response.success,
            "the `{key}` named form failed: {:?}",
            response.error
        );
        assert_eq!(response.result, Some(json!(5)), "the `{key}` named form");
    }

    // The case the shim exists for: `{"path": "."}` documented as `ls(".")`.
    let listed = call("ls", json!({ "path": "." }));
    assert!(
        listed.success,
        "ls({{\"path\": \".\"}}) failed: {:?}",
        listed.error
    );
    assert!(
        listed.result.is_some_and(|v| v != JsonValue::Null),
        "ls listed nothing at all"
    );
}

#[test]
fn an_object_with_no_named_field_is_one_record_argument() {
    // Every field survives: a record is one value, not a list of arguments.
    let response = call("len", json!({ "a": 1, "b": 2, "c": 3 }));
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!(3)));

    // And the record reaches the builtin with its keys intact, including a key
    // the language could not spell as a field access.
    let stringified = call("json_stringify", json!({ "x-y": 1, "z": [2, null] }));
    assert!(stringified.success, "failed: {:?}", stringified.error);
    let text = stringified
        .result
        .as_ref()
        .and_then(|v| v.as_str())
        .expect("json_stringify returns text");
    let back: JsonValue = serde_json::from_str(text).expect("the text is JSON");
    assert_eq!(back, json!({ "x-y": 1, "z": [2, null] }));
}

#[test]
fn an_object_mixing_a_named_field_with_others_is_refused_rather_than_trimmed() {
    for args in [
        json!({ "input": "hello", "extra": 1 }),
        json!({ "path": ".", "x": 2 }),
        json!({ "value": 1, "input": 2 }),
    ] {
        let response = call("len", args.clone());
        assert!(!response.success, "`{args}` must not be read as a call");
        assert_eq!(code(&response), "E_BAD_ARG", "`{args}`");
        let text = response.error.unwrap_or_default();
        assert!(
            text.contains("positional argument array"),
            "the refusal must name the form that works: {text}"
        );
    }
}

#[test]
fn a_record_inside_the_argument_array_keeps_every_field() {
    // The positional array is the faithful channel: nothing here is a name.
    // `json_stringify` reads its first argument, so the text it returns is the
    // record itself — proof it arrived as one argument with every field, not as
    // a field list.
    let response = call(
        "json_stringify",
        json!([{ "path": ".", "x": [1, null, { "y": 2 }] }]),
    );
    assert!(response.success, "failed: {:?}", response.error);
    let text = response
        .result
        .as_ref()
        .and_then(|v| v.as_str())
        .expect("json_stringify returns text");
    let back: JsonValue = serde_json::from_str(text).expect("the text is JSON");
    assert_eq!(back, json!({ "path": ".", "x": [1, null, { "y": 2 }] }));

    // One argument, so its field count is what `len` reads.
    let counted = call("len", json!([{ "path": ".", "x": 1 }]));
    assert_eq!(counted.result, Some(json!(2)));
}

// ── Data stays data ──────────────────────────────────────────────────────

/// The one string that would be executable if arguments were still rendered
/// into source: an interpolation naming a real write, into a path this test
/// owns.
fn interpolation_probe(path: &Path) -> String {
    format!(
        "${{file.write(\"{}\", \"typed-api-must-not-run-this\")}}",
        path.display()
    )
}

#[test]
fn a_string_containing_an_interpolation_is_never_evaluated() {
    let dir = TempDir::new("inert");
    let marker = dir.path().join("written-by-a-probe.txt");
    let probe = interpolation_probe(&marker);

    // As an argument to a call: the value comes back byte for byte.
    let stringified = call("json_stringify", json!({ "probe": probe.clone() }));
    assert!(stringified.success, "failed: {:?}", stringified.error);
    let text = stringified
        .result
        .as_ref()
        .and_then(|v| v.as_str())
        .expect("json_stringify returns text");
    let back: JsonValue = serde_json::from_str(text).expect("the text is JSON");
    assert_eq!(
        back["probe"].as_str(),
        Some(probe.as_str()),
        "the argument was altered on the way in"
    );

    // As a positional argument: `len` measures the string it was handed.
    let measured = call("len", json!([probe.clone()]));
    assert!(measured.success, "failed: {:?}", measured.error);
    assert_eq!(measured.result, Some(json!(probe.len() as i64)));

    // As a pipeline seed: the seed is a value, not a program.
    let seeded = pipeline(vec![step("len", json!(null))], Some(json!(probe.clone())));
    assert!(seeded.success, "failed: {:?}", seeded.error);
    assert_eq!(seeded.result, Some(json!(probe.len() as i64)));

    // The negative half: nothing ran. A source-rendering implementation would
    // have interpolated the probe and written this file.
    assert!(
        !marker.exists(),
        "the probe ran: {} exists",
        marker.display()
    );
}

#[test]
fn a_value_that_looks_like_an_expression_is_still_a_value() {
    // The same guarantee for text that would parse as a predicate or a program.
    for text in ["n > 1", "1 + 1", "fn(x) => x", "${sys.hostname()}"] {
        let response = call("type_of", json!([text]));
        assert!(
            response.success,
            "failed for {text:?}: {:?}",
            response.error
        );
        assert_eq!(response.result, Some(json!("String")), "for {text:?}");

        let seeded = pipeline(vec![step("type_of", json!(null))], Some(json!(text)));
        assert_eq!(seeded.result, Some(json!("String")), "seeded {text:?}");
    }
}

// ── Transport metadata ───────────────────────────────────────────────────

#[test]
fn a_typed_envelope_says_so_and_never_claims_source_ran() {
    let called = call("len", json!(["hello"]));
    assert!(called.success);
    let meta = metadata(&called);
    assert_eq!(meta["structured_data"], json!("typed-v1"));
    assert_eq!(meta["action"], json!("call"));
    assert_eq!(meta["builtin"], json!("len"));
    for echo in ["code_executed", "code_attempted"] {
        assert!(
            meta.get(echo).is_none(),
            "a typed call reports `{echo}`: {meta}"
        );
    }
    assert!(
        meta.get("token_accounting").is_some(),
        "no accounting: {meta}"
    );

    let piped = pipeline(vec![step("len", json!(null))], Some(json!("hello")));
    assert!(piped.success, "failed: {:?}", piped.error);
    assert_eq!(piped.result, Some(json!(5)));
    let meta = metadata(&piped);
    assert_eq!(meta["structured_data"], json!("typed-v1"));
    assert_eq!(meta["action"], json!("pipeline"));
    assert!(meta.get("code_executed").is_none(), "{meta}");

    // `eval` is the source surface, and it still reports the source it ran.
    let evaluated = process_request(&AgentRequest::Eval {
        code: "1 + 1".to_string(),
    });
    assert_eq!(evaluated.result, Some(json!(2)));
    assert_eq!(metadata(&evaluated)["code_executed"], json!("1 + 1"));
}

#[test]
fn a_typed_failure_is_coded_and_names_what_failed() {
    let response = call("no_such_builtin_9f3a", json!([]));
    assert!(!response.success);
    assert_eq!(code(&response), "E_UNKNOWN_BUILTIN");
    let meta = metadata(&response);
    assert_eq!(meta["structured_data"], json!("typed-v1"));
    assert_eq!(meta["action"], json!("call"));
    assert_eq!(meta["builtin"], json!("no_such_builtin_9f3a"));
    assert_eq!(meta["code"], json!("E_UNKNOWN_BUILTIN"));
    for echo in ["code_executed", "code_attempted"] {
        assert!(
            meta.get(echo).is_none(),
            "a typed failure reports `{echo}`: {meta}"
        );
    }
}

#[test]
fn a_native_failure_is_an_envelope_and_not_a_process_error() {
    // The command-line surface answers with `success: false` and exits 0, which
    // is the difference between a request that failed and a protocol that did.
    let text = process_json_request(r#"{"action":"call","builtin":"nope_9f3a","args":[]}"#)
        .expect("a failed call is still a readable envelope");
    let parsed: AgentResponse = serde_json::from_str(&text).expect("envelope");
    assert!(!parsed.success);
    assert_eq!(code(&parsed), "E_UNKNOWN_BUILTIN");
}

// ── Builtin names ────────────────────────────────────────────────────────

#[test]
fn an_unknown_or_dotted_name_is_an_unknown_builtin_and_never_parsed() {
    for name in ["no_such_builtin_9f3a", "crypto.hash", "file.read", ""] {
        let response = call(name, json!([]));
        assert!(!response.success, "`{name}` must not resolve");
        assert_eq!(code(&response), "E_UNKNOWN_BUILTIN", "`{name}`");
    }
}

// ── Numbers ──────────────────────────────────────────────────────────────

#[test]
fn every_native_representable_number_keeps_its_exact_value() {
    assert_eq!(
        call("type_of", json!([i64::MAX])).result,
        Some(json!("Int"))
    );
    assert_eq!(
        call("type_of", json!([i64::MIN])).result,
        Some(json!("Int"))
    );
    assert_eq!(
        call("json_stringify", json!([i64::MAX])).result,
        Some(json!("9223372036854775807")),
        "an integer that a float could not carry must not become one"
    );
    assert_eq!(call("type_of", json!([0])).result, Some(json!("Int")));
    assert_eq!(call("type_of", json!([-1.5])).result, Some(json!("Float")));
}

#[test]
fn a_number_with_no_exact_native_form_is_refused_rather_than_rounded() {
    let above_i64 = i64::MAX as u64 + 1;
    for number in [json!(above_i64), json!(above_i64 + 12_345), json!(u64::MAX)] {
        let response = call("type_of", json!([number.clone()]));
        assert!(!response.success, "`{number}` must not be accepted");
        assert_eq!(code(&response), "E_BAD_ARG", "`{number}`");
    }

    // Nested inside the argument, the same refusal — no silent rounding deeper
    // in the value.
    let nested = call("type_of", json!([[{ "n": u64::MAX }]]));
    assert!(!nested.success);
    assert_eq!(code(&nested), "E_BAD_ARG");

    // A pipeline seed is checked the same way.
    let seeded = pipeline(vec![step("type_of", json!(null))], Some(json!(u64::MAX)));
    assert!(!seeded.success);
    assert_eq!(code(&seeded), "E_BAD_ARG");
}

// ── Value fidelity through a call ────────────────────────────────────────

#[test]
fn nulls_control_characters_and_non_identifier_keys_survive_a_call() {
    // One document holding every shape the source channel could not carry: a
    // null, a control character, a key outside the identifier syntax, a reserved
    // word, a numeric key, and nested arrays.
    let document = r#"{"a":null,"b":"\u0001\t\"q\"","c":{"x-y":[1,null,[2,{"a.b":3}]]},"__proto__":{"z":null},"let":1,"0":2}"#;
    let expected: JsonValue = serde_json::from_str(document).expect("the fixture is JSON");

    let response = call("json_parse", json!([document]));
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result_type.as_deref(), Some("Record"));
    assert_eq!(
        response.result,
        Some(expected),
        "a value did not survive the trip"
    );
}

// ── Pipelines: steps, seeds, select and predicate ────────────────────────

#[test]
fn an_empty_pipeline_is_a_coded_argument_failure() {
    let response = pipeline(vec![], None);
    assert!(!response.success);
    assert_eq!(code(&response), "E_BAD_ARG");
}

#[test]
fn a_step_keeps_its_arguments_and_consumes_the_previous_result() {
    // No seed: the first step's own arguments are what it runs with.
    let first = pipeline(
        vec![
            step("json_parse", json!([r#"[{"n":1},{"n":2}]"#])),
            step("len", json!(null)),
        ],
        None,
    );
    assert!(first.success, "failed: {:?}", first.error);
    assert_eq!(first.result, Some(json!(2)));

    // With a seed, the same step reads the piped value instead.
    let second = pipeline(vec![step("len", json!(null))], Some(json!(["a", "b", "c"])));
    assert!(second.success, "failed: {:?}", second.error);
    assert_eq!(second.result, Some(json!(3)));
}

#[test]
fn select_reads_a_literal_field_path_and_never_source() {
    let cases: &[(&str, &str)] = &[
        // A key the language cannot spell as a field access.
        (r#"[{"a-b":1},{"a-b":2}]"#, "a-b"),
        // A Unicode key.
        (r#"[{"名前":"x"},{"名前":"y"}]"#, "名前"),
        // A reserved word.
        (r#"[{"let":1}]"#, "let"),
        // A nested path: each segment is one literal key.
        (r#"[{"meta":{"size":7}},{"meta":{"size":8}}]"#, "meta.size"),
    ];

    for (document, path) in cases {
        let mut selected = step("json_parse", json!([document]));
        selected.select = Some((*path).to_string());
        let response = pipeline(vec![selected], None);
        assert!(
            response.success,
            "select {path:?} failed: {:?}",
            response.error
        );
        let items = response
            .result
            .as_ref()
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("select {path:?} did not return an array"));
        assert!(!items.is_empty(), "select {path:?} returned nothing");
        assert!(
            items.iter().all(|v| v != &JsonValue::Null),
            "select {path:?} invented a null"
        );
    }

    // The exact values, so nothing is merely shaped right.
    let mut selected = step("json_parse", json!([r#"[{"a-b":1},{"a-b":2}]"#]));
    selected.select = Some("a-b".to_string());
    assert_eq!(pipeline(vec![selected], None).result, Some(json!([1, 2])));
}

#[test]
fn an_unknown_select_field_is_an_error_and_never_a_null() {
    let mut selected = step("json_parse", json!([r#"[{"name":"a"}]"#]));
    selected.select = Some("nope".to_string());
    let response = pipeline(vec![selected], None);
    assert!(!response.success, "a missing field must not answer null");
    assert_eq!(code(&response), "E_UNKNOWN_FIELD");
    assert!(
        response.error.unwrap_or_default().contains("nope"),
        "the refusal must name the field"
    );

    // A subject that is not an array is refused the way `map` refused it, which
    // is what the historical `| map(fn(x) => x.<path>)` did.
    let mut on_record = step("json_parse", json!([r#"{"a":1}"#]));
    on_record.select = Some("a".to_string());
    let response = pipeline(vec![on_record], None);
    assert!(!response.success);
    assert_eq!(code(&response), "E_BAD_ARG");
}

#[test]
fn a_predicate_is_evaluated_as_the_expression_it_names() {
    // `where` reads its predicate from `predicate`, so the array it filters
    // comes from the step before it.
    let mut filtered = step("where", json!(null));
    filtered.predicate = Some("n > 1".to_string());
    let response = pipeline(
        vec![
            step("json_parse", json!([r#"[{"n":1},{"n":3}]"#])),
            filtered,
        ],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([{ "n": 3 }])));

    // A predicate and a selection together: the predicate keeps the elements,
    // then the selection reads one field out of each.
    let mut both = step("where", json!(null));
    both.predicate = Some("n > 1".to_string());
    both.select = Some("n".to_string());
    let response = pipeline(
        vec![step("json_parse", json!([r#"[{"n":1},{"n":3}]"#])), both],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([3])));

    // A predicate that is true for everything keeps everything.
    let mut all = step("where", json!(null));
    all.predicate = Some("true".to_string());
    let response = pipeline(vec![step("json_parse", json!(["[1,2]"])), all], None);
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([1, 2])));

    // The form the shipped compatibility script sends: a field comparison
    // shorthand paired with a selection.
    let mut compat = step("where", json!(null));
    compat.predicate = Some("size > 2".to_string());
    compat.select = Some("size".to_string());
    let response = pipeline(
        vec![
            step(
                "json_parse",
                json!([r#"[{"size":1},{"size":5},{"size":9}]"#]),
            ),
            compat,
        ],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([5, 9])));
}

#[test]
fn a_predicate_written_against_the_element_is_used_as_written() {
    // The element itself. This is the spelling a caller reads out of the tool
    // documentation (`fn(x) => <predicate>`), and the field shorthand must not
    // prefix it a second time into `x.x`.
    let mut element = step("where", json!(null));
    element.predicate = Some("x > 2".to_string());
    let response = pipeline(vec![step("json_parse", json!(["[1,2,3]"])), element], None);
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([3])));

    // Rooted at the element, one field down.
    let mut field = step("where", json!(null));
    field.predicate = Some("x.size > 2".to_string());
    let response = pipeline(
        vec![
            step(
                "json_parse",
                json!([r#"[{"size":1},{"size":5},{"size":9}]"#]),
            ),
            field,
        ],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([{ "size": 5 }, { "size": 9 }])));

    // The same field, named with a leading dot instead.
    let mut dotted = step("where", json!(null));
    dotted.predicate = Some(".size > 2".to_string());
    let response = pipeline(
        vec![
            step(
                "json_parse",
                json!([r#"[{"size":1},{"size":5},{"size":9}]"#]),
            ),
            dotted,
        ],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([{ "size": 5 }, { "size": 9 }])));

    // A record field that really is named `x` stays reachable through the
    // element, which is the only reading that was ever valid for this shape.
    let mut named_x = step("where", json!(null));
    named_x.predicate = Some("x.x > 1".to_string());
    let response = pipeline(
        vec![step("json_parse", json!([r#"[{"x":1},{"x":3}]"#])), named_x],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([{ "x": 3 }])));

    // The shorthand's token boundary is the identifier, so a longer name is
    // still a field shorthand.
    let mut xylophone = step("where", json!(null));
    xylophone.predicate = Some("xylophone > 1".to_string());
    let response = pipeline(
        vec![
            step(
                "json_parse",
                json!([r#"[{"xylophone":1},{"xylophone":3}]"#]),
            ),
            xylophone,
        ],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([{ "xylophone": 3 }])));

    // A whole lambda is passed through untouched.
    let mut lambda = step("where", json!(null));
    lambda.predicate = Some("fn(e) => e.n > 1".to_string());
    let response = pipeline(
        vec![step("json_parse", json!([r#"[{"n":1},{"n":3}]"#])), lambda],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([{ "n": 3 }])));
}

#[test]
fn a_predicate_step_does_not_convert_the_arguments_it_replaces() {
    // A predicate is the lambda the builtin takes, so `args` is not this call's
    // argument list. Refusing a value the call would never have seen would be a
    // failure with no cause.
    let mut replaced = step("where", json!([u64::MAX]));
    replaced.predicate = Some("size > 2".to_string());
    let response = pipeline(
        vec![
            step("json_parse", json!([r#"[{"size":1},{"size":5}]"#])),
            replaced,
        ],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!([{ "size": 5 }])));
}

#[test]
fn a_failing_predicate_propagates_instead_of_being_ignored() {
    let mut broken = step("where", json!(null));
    broken.predicate = Some("n >".to_string());
    let response = pipeline(
        vec![step("json_parse", json!([r#"[{"n":1}]"#])), broken],
        None,
    );
    assert!(!response.success, "an unparseable predicate must fail");
    assert_eq!(code(&response), "E_BAD_ARG");
}

// ── Preflight: the description is validated before anything runs ─────────

/// A step that really writes, so "the file was not written" is a claim about
/// this environment rather than a tautology.
fn write_step(path: &Path) -> PipelineStep {
    let target = path.to_string_lossy().into_owned();
    step("save_json", json!([target, { "written": true }]))
}

/// The same path as it must appear inside a predicate, which *is* source: the
/// AE lexer reads backslashes, so a Windows path travels with forward slashes.
fn source_path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[test]
fn a_write_step_really_writes_through_the_typed_pipeline() {
    // The control for the refusals below.
    let dir = TempDir::new("preflight-control");
    let marker = dir.path().join("control.json");
    let response = pipeline(
        vec![write_step(&marker), step("type_of", json!(null))],
        None,
    );
    assert!(response.success, "failed: {:?}", response.error);
    assert_eq!(response.result, Some(json!("Record")));
    assert!(
        marker.exists(),
        "the control write did not happen, so the refusals below prove nothing"
    );
}

#[test]
fn a_later_unparseable_predicate_runs_no_earlier_step() {
    let dir = TempDir::new("preflight-predicate");
    let marker = dir.path().join("written.json");
    let mut broken = step("where", json!(null));
    broken.predicate = Some("n >".to_string());

    let response = pipeline(vec![write_step(&marker), broken], None);
    assert!(!response.success);
    assert_eq!(code(&response), "E_BAD_ARG");
    assert!(
        !marker.exists(),
        "a later bad description ran the earlier write"
    );
}

#[test]
fn a_later_empty_select_segment_runs_no_earlier_step() {
    let dir = TempDir::new("preflight-select");
    let marker = dir.path().join("written.json");
    let mut bad_select = step("json_parse", json!(["[1]"]));
    bad_select.select = Some("a..b".to_string());

    let response = pipeline(vec![write_step(&marker), bad_select], None);
    assert!(!response.success);
    assert_eq!(code(&response), "E_BAD_ARG");
    assert!(
        !marker.exists(),
        "a later bad description ran the earlier write"
    );
}

#[test]
fn a_later_ambiguous_argument_object_runs_no_earlier_step() {
    let dir = TempDir::new("preflight-args");
    let marker = dir.path().join("written.json");
    let ambiguous = step("len", json!({ "input": 1, "extra": 2 }));

    let response = pipeline(vec![write_step(&marker), ambiguous], None);
    assert!(!response.success);
    assert_eq!(code(&response), "E_BAD_ARG");
    assert!(
        !marker.exists(),
        "a later bad description ran the earlier write"
    );
}

#[test]
fn an_unrepresentable_seed_or_argument_runs_no_step() {
    // In a later step's arguments.
    let dir = TempDir::new("preflight-number");
    let marker = dir.path().join("later-argument.json");
    let response = pipeline(
        vec![write_step(&marker), step("type_of", json!([u64::MAX]))],
        None,
    );
    assert!(!response.success);
    assert_eq!(code(&response), "E_BAD_ARG");
    assert!(
        !marker.exists(),
        "an argument refused in preflight ran the earlier write"
    );

    // And as the seed, which is converted once, before any dispatch.
    let seeded = dir.path().join("seed.json");
    let response = pipeline(vec![write_step(&seeded)], Some(json!(u64::MAX)));
    assert!(!response.success);
    assert_eq!(code(&response), "E_BAD_ARG");
    assert!(!seeded.exists(), "a refused seed ran the write step");
}

#[test]
fn constructing_a_predicate_runs_nothing_inside_it() {
    let dir = TempDir::new("preflight-source");
    let marker = dir.path().join("written.json");
    let inner = dir.path().join("inner.json");
    let mut polyglot = step("where", json!(null));
    // Three statements: a lambda, a real write, and a lambda again. The last is
    // what evaluating the whole program would return, so a construction that ran
    // it would accept this predicate *and* have written `inner`.
    polyglot.predicate = Some(format!(
        "fn(x) => x > 0\nsave_json(\"{}\", 1)\nfn(x) => true",
        source_path_string(&inner)
    ));

    let response = pipeline(vec![write_step(&marker), polyglot], None);
    assert!(!response.success);
    assert_eq!(code(&response), "E_BAD_ARG");
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("one lambda expression"),
        "refused by the single-statement rule, not by a parse error"
    );
    assert!(
        !inner.exists(),
        "constructing the predicate ran one of its statements"
    );
    assert!(
        !marker.exists(),
        "a later bad description ran the earlier write"
    );
}

// ── The surrounding surfaces are unchanged ───────────────────────────────

#[test]
fn the_discovery_actions_still_answer() {
    for request in [
        AgentRequest::ListBuiltins { category: None },
        AgentRequest::Describe {
            builtin: "len".to_string(),
        },
        AgentRequest::TypeInfo { type_name: None },
        AgentRequest::Schema {
            format: aethershell::agent_api::SchemaFormat::Ontology,
        },
    ] {
        let response = process_request(&request);
        assert!(response.success, "failed: {:?}", response.error);
    }
}

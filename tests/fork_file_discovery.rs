//! File-operation discovery describes arguments, effects and success distinctions.
use aethershell::{agent_api, builtins, modules, parser, safety, signature, value::Value};
use serde_json::json;

#[test]
fn common_file_operations_declare_their_real_positional_arguments() {
    for (name, parameters, returns) in [
        ("read_text", vec!["path"], "String"),
        ("file_read_text", vec!["path", "options"], "Record"),
        (
            "file_write_verified",
            vec!["path", "content", "options"],
            "Record",
        ),
        ("file_write", vec!["path", "content"], "Record"),
        ("file_append", vec!["path", "content"], "Record"),
        (
            "file_replace",
            vec!["path", "old_text", "new_text"],
            "Record",
        ),
        ("file_patch", vec!["path", "patches"], "Record"),
    ] {
        assert!(builtins::is_dispatched(name), "{name}");
        let declared = signature::signature_of(name).expect("declared file operation");
        assert_eq!(
            declared
                .params
                .iter()
                .map(|parameter| parameter.name)
                .collect::<Vec<_>>(),
            parameters,
            "{name}"
        );
        let description = agent_api::ontology_describe_json(name);
        assert_eq!(description["return_type"], returns, "{name}");
        assert_eq!(description["signature"], declared.render(), "{name}");
        let expected = if name == "read_text" || name == "file_read_text" {
            safety::Effect::ReadLocal
        } else {
            safety::Effect::WriteLocal
        };
        assert_eq!(safety::effect_of(name), expected, "{name}");
    }
}

#[test]
fn write_aliases_share_the_same_declared_contract() {
    for (name, aliases) in [
        ("file_write", vec!["write_file", "text_write"]),
        ("file_append", vec!["append_file"]),
    ] {
        let expected = signature::signature_of(name).expect("canonical signature");
        for alias in aliases {
            assert_eq!(
                signature::signature_of(alias)
                    .expect("alias signature")
                    .name,
                expected.name
            );
        }
    }
    let Value::Record(file) = modules::file_module() else {
        panic!("file module")
    };
    for (method, target) in [
        ("read_text", "file_read_text"),
        ("write_verified", "file_write_verified"),
    ] {
        let Some(Value::Builtin(reference)) = file.get(method) else {
            panic!("module method {method}")
        };
        assert_eq!(reference.name, target);
    }
}

#[test]
fn file_refusal_examples_execute_without_any_file_effect() {
    for name in [
        "file_read_text",
        "file_write_verified",
        "file_write",
        "file_append",
        "file_replace",
    ] {
        let declared = signature::signature_of(name).expect("declared operation");
        for (source, expected) in declared.examples {
            let program = parser::parse_program(source).expect("example parses");
            let mut env = modules::env_with_modules();
            assert_eq!(
                aethershell::eval::eval_program(&program, &mut env)
                    .expect("validation refusal is catchable"),
                Value::Str((*expected).into()),
                "{name}: {source}"
            );
        }
    }
}

#[test]
fn legacy_write_and_append_keep_their_exact_behavior_and_result_fields() {
    let root = tempfile::tempdir().expect("private fixture directory");
    let path = root.path().join("中文.txt");
    let path = path.to_string_lossy();
    let written = agent_api::process_request(&agent_api::AgentRequest::Call {
        builtin: "file_write".into(),
        args: json!([path, ["甲", "乙"]]),
    });
    assert!(written.success, "{:?}", written.error);
    let result = written.result.expect("write result");
    assert_eq!(result["bytes"], 7);
    assert_eq!(result["lines"], 2);
    assert_eq!(result["success"], true);
    assert!(
        result.get("verified").is_none(),
        "legacy completion is not new verification"
    );
    assert_eq!(
        std::fs::read(root.path().join("中文.txt")).expect("external read"),
        "甲\n乙".as_bytes()
    );
    let appended = agent_api::process_request(&agent_api::AgentRequest::Call {
        builtin: "file_append".into(),
        args: json!([path, "丙\r\n"]),
    });
    assert!(appended.success, "{:?}", appended.error);
    assert_eq!(appended.result.expect("append result")["bytes_appended"], 5);
    assert_eq!(
        std::fs::read(root.path().join("中文.txt")).expect("external read"),
        "甲\n乙丙\r\n".as_bytes()
    );
}

#[test]
fn a_successful_api_envelope_does_not_hide_a_partial_legacy_patch() {
    let root = tempfile::tempdir().expect("private fixture directory");
    let path = root.path().join("partial.txt");
    std::fs::write(&path, "DEBUG=0\r\n").expect("fixture");
    let response = agent_api::process_request(&agent_api::AgentRequest::Call {
        builtin: "file_patch".into(),
        args: json!([path.to_string_lossy(), [{"find":"DEBUG=0","replace":"DEBUG=1"}, {"find":"NOT_PRESENT","replace":"unused"}]]),
    });
    assert!(response.success, "{:?}", response.error);
    let result = response.result.expect("partial result");
    assert_eq!(result["success"], false);
    assert_eq!(result["patches_applied"], 1);
    assert_eq!(result["patches_failed"], 1);
    assert_eq!(std::fs::read(&path).expect("actual bytes"), b"DEBUG=1\r\n");
    assert!(signature::signature_of("file_patch")
        .expect("signature")
        .doc
        .contains("valid patches may be written"));
}

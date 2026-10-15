//! The explicit process runner remains an exec operation before any program starts.
use aethershell::{builtins, modules, parser, safety, value::Value};

#[test]
fn process_module_and_effect_classification_match_the_runner() {
    let Value::Record(process) = modules::proc_module() else {
        panic!("proc is a module")
    };
    let Some(Value::Builtin(run)) = process.get("run") else {
        panic!("proc.run is a builtin")
    };
    assert_eq!(run.name, "proc_run");
    assert!(builtins::is_dispatched(&run.name));
    assert!(safety::effect_is_declared(&run.name));
    assert_eq!(safety::effect_of(&run.name), safety::Effect::Exec);
}

#[test]
fn invalid_call_refuses_before_a_missing_program_can_be_started() {
    let mut env = modules::env_with_modules();
    let unknown = Value::Str("no_such_aether_encoding_probe_program".into());
    for options in [
        Value::Record(
            [("encoding".into(), Value::Str("guess".into()))]
                .into_iter()
                .collect(),
        ),
        Value::Record([("timeout_ms".into(), Value::Int(0))].into_iter().collect()),
        Value::Record(
            [("max_output_bytes".into(), Value::Int(0))]
                .into_iter()
                .collect(),
        ),
        Value::Record(
            [("unknown".into(), Value::Bool(true))]
                .into_iter()
                .collect(),
        ),
    ] {
        let error = builtins::call(
            "proc_run",
            vec![Value::Array(vec![unknown.clone()]), options],
            &mut env,
        )
        .expect_err("invalid options must be refused");
        assert!(error.to_string().contains("E_BAD_ARG"), "{error}");
        assert!(
            !error.to_string().contains("E_TOOL_MISSING"),
            "the process lookup ran before validation: {error}"
        );
    }
}

#[test]
fn module_refusal_is_available_without_a_process_effect() {
    let program = parser::parse_program("try { proc.run([]) } catch e { e.error.code }")
        .expect("module form parses");
    let mut env = modules::env_with_modules();
    assert_eq!(
        aethershell::eval::eval_program(&program, &mut env).expect("empty argv is catchable"),
        Value::Str("E_BAD_ARG".into())
    );
}

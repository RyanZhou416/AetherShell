//! Agent discovery keeps literal-text guidance alongside executable syntax examples.
use aethershell::{
    agent_api, builtins, modules, parser, signature, syntax_kb::SyntaxKB, value::Value,
};

#[test]
fn raw_string_forms_are_visible_in_help_and_the_language_reference() {
    let mut env = modules::env_with_modules();
    let help = builtins::call("help", vec![], &mut env).expect("help");
    let Value::Str(text) = help else {
        panic!("help must return text")
    };
    assert!(text.contains(r#"r"C:\temp\new""#));
    assert!(text.contains("unknown escapes"));
    assert!(text.contains("proc_run"));
    let kb = SyntaxKB::new();
    let entry = kb.get("aethershell").expect("language reference");
    assert!(entry.specification.contains("STRING LITERALS"));
    assert!(entry.specification.contains("no ${...} interpolation"));
}

#[test]
fn string_type_examples_include_a_raw_literal_that_evaluates_verbatim() {
    let ontology = agent_api::build_language_ontology();
    let string = ontology
        .types
        .iter()
        .find(|kind| kind.name == "String")
        .expect("String type");
    assert!(string.description.contains("raw"));
    let raw = string
        .examples
        .iter()
        .find(|code| code.starts_with("r\""))
        .expect("raw String example");
    let program = parser::parse_program(raw).expect("raw example parses");
    let mut env = modules::env_with_modules();
    assert_eq!(
        aethershell::eval::eval_program(&program, &mut env).expect("raw example evaluates"),
        Value::Str(r"C:\temp\new".into())
    );
}

#[test]
fn process_runner_is_dispatched_discoverable_and_exec_classified() {
    assert!(builtins::is_dispatched("proc_run"));
    assert_eq!(
        aethershell::safety::effect_of("proc_run"),
        aethershell::safety::Effect::Exec
    );
    let declared = signature::signature_of("proc_run").expect("declared runner");
    assert_eq!(declared.params[0].name, "argv");
    assert_eq!(declared.params[0].ty, signature::Ty::Array);
    assert_eq!(declared.params[1].name, "options");
    assert_eq!(declared.params[1].ty, signature::Ty::Record);
    let detail = agent_api::ontology_describe_json("proc_run");
    assert_eq!(detail["effect"], "exec");
    let mut env = modules::env_with_modules();
    for (source, expected) in declared.examples {
        let program = parser::parse_program(source).expect("refusal example parses");
        let value = aethershell::eval::eval_program(&program, &mut env)
            .expect("refusal example runs without spawning");
        assert_eq!(value, Value::Str((*expected).into()));
    }
    assert!(declared.doc.contains("base64"));
    assert!(declared.doc.contains("strict"));
}

//! The discovery contract of the two builtins whose catalogue entries did not
//! describe their arguments: `proc_spawn` and `file_patch`.
//!
//! Both were advertised from their names alone — `proc_spawn() -> Value`,
//! `file_patch() -> Value` — so an agent was told neither took an argument while
//! each body read positional ones. This pins the corrected field set, the
//! published tool parameters, the valid shapes, and the coded refusals.
//!
//! `proc_spawn` is never invoked with a program: a runnable example would launch
//! a process on every machine that runs the suite. Its valid shapes are proved
//! through `signature::validate`, the gate the dispatcher runs before a body.
//! `file_patch` edits a private temp file this test owns. Effect classification
//! and the gate itself live in `tests/exec_gate_coverage.rs` and
//! `tests/write_evidence.rs`.

use aethershell::agent_api::ontology_describe_json;
use aethershell::builtins::{call, call_with_input, is_dispatched};
use aethershell::env::Env;
use aethershell::signature::{signature_of, validate, Signature, Ty};
use aethershell::value::Value;

fn declared(name: &str) -> &'static Signature {
    assert!(is_dispatched(name), "{name} is not dispatched at all");
    signature_of(name).unwrap_or_else(|| {
        panic!("{name} has no declaration, so its catalogue entry is derived from its name")
    })
}

/// The published tool parameters of `name`, in declaration order:
/// `(name, type, required)`.
fn published_params(name: &str) -> Vec<(String, String, bool)> {
    ontology_describe_json(name)["parameters"]
        .as_array()
        .map(|params| {
            params
                .iter()
                .map(|p| {
                    (
                        p["name"].as_str().unwrap_or_default().to_string(),
                        p["type"].as_str().unwrap_or_default().to_string(),
                        p["required"].as_bool().unwrap_or(false),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One `{find, replace}` patch exactly as the body reads it.
fn patch(find: &str, replace: &str) -> Value {
    Value::Record(
        [
            ("find".to_string(), Value::Str(find.into())),
            ("replace".to_string(), Value::Str(replace.into())),
        ]
        .into_iter()
        .collect(),
    )
}

fn patch_array(find: &str, replace: &str) -> Value {
    Value::Array(vec![patch(find, replace)])
}

/// Every declared example, run: `(code, what it answered)`.
fn run_examples(name: &str) -> Vec<(String, Value)> {
    let sig = declared(name);
    assert!(!sig.examples.is_empty(), "{name} declares no example");
    sig.examples
        .iter()
        .map(|(code, want)| {
            let mut env = aethershell::modules::env_with_modules();
            let program =
                aethershell::parser::parse_program(code).unwrap_or_else(|e| panic!("{code}: {e}"));
            let got = aethershell::eval::eval_program(&program, &mut env)
                .unwrap_or_else(|e| panic!("the example did not run: {code}\n{e}"));
            assert_eq!(
                got,
                Value::Str((*want).to_string()),
                "the example does not produce the refusal it claims: {code}"
            );
            ((*code).to_string(), got)
        })
        .collect()
}

#[test]
fn proc_spawn_declares_a_program_and_string_arguments() {
    let sig = declared("proc_spawn");
    assert_eq!(sig.returns, "Int", "proc_spawn returns the child pid");
    assert!(
        sig.subject.is_none(),
        "proc_spawn takes its program as the first argument, not through the pipe"
    );
    assert_eq!(sig.params.len(), 2, "declares {}", sig.params.len());

    assert_eq!(sig.params[0].name, "program");
    assert_eq!(sig.params[0].ty, Ty::Str);
    assert!(sig.params[0].required, "a program is what the call is for");
    assert_eq!(sig.params[1].name, "args");
    assert_eq!(sig.params[1].ty, Ty::Str);
    assert!(
        sig.params[1].variadic && !sig.params[1].required,
        "`proc_spawn(\"git\")` and `proc_spawn(\"git\", \"status\")` are both calls the body reads"
    );

    assert_eq!(
        sig.render(),
        "proc_spawn(program: String, args...: String) -> Int"
    );
    let def = ontology_describe_json("proc_spawn");
    assert_eq!(def["signature"].as_str(), Some(sig.render().as_str()));
    assert_eq!(
        published_params("proc_spawn"),
        vec![
            ("program".to_string(), "String".to_string(), true),
            ("args".to_string(), "String".to_string(), false),
        ]
    );
    assert!(
        sig.doc.contains("pid") && sig.doc.contains("proc_spawn("),
        "the description must carry the return value and the valid shape, since no \
         example may run one: {}",
        sig.doc
    );
}

#[test]
fn file_patch_declares_a_path_and_the_patch_argument() {
    let sig = declared("file_patch");
    assert_eq!(sig.returns, "Record");
    assert!(
        sig.subject.is_none(),
        "the piped subject is the patches, not the path, so it is not a declared subject"
    );
    assert_eq!(sig.params.len(), 2, "declares {}", sig.params.len());

    assert_eq!(sig.params[0].name, "path");
    assert_eq!(sig.params[0].ty, Ty::Str);
    assert!(
        sig.params[0].required,
        "without a path there is nothing to edit"
    );
    assert_eq!(sig.params[1].name, "patches");
    assert_eq!(
        sig.params[1].ty,
        Ty::Any,
        "the body accepts one patch record or an array of them, so no single Ty names it"
    );
    assert!(
        !sig.params[1].required,
        "the patches may arrive through the pipe instead of as the second argument"
    );
    assert!(
        sig.params[1].doc.contains("array") && sig.params[1].doc.contains("record"),
        "the parameter doc must name both accepted shapes: {}",
        sig.params[1].doc
    );

    assert_eq!(
        sig.render(),
        "file_patch(path: String, patches?: Any) -> Record"
    );
    let def = ontology_describe_json("file_patch");
    assert_eq!(def["signature"].as_str(), Some(sig.render().as_str()));
    assert_eq!(
        published_params("file_patch"),
        vec![
            ("path".to_string(), "String".to_string(), true),
            ("patches".to_string(), "Any".to_string(), false),
        ]
    );
    assert!(
        sig.doc.contains("find") && sig.doc.contains("replace") && sig.doc.contains("file_patch("),
        "the description must name the record fields and the valid shape: {}",
        sig.doc
    );
}

#[test]
fn every_valid_declared_shape_passes_validation() {
    // The field-set proof, not a claim about what the OS does with the call.
    assert!(validate("proc_spawn", &[Value::Str("git".into())], None).is_ok());
    assert!(
        validate(
            "proc_spawn",
            &[
                Value::Str("git".into()),
                Value::Str("status".into()),
                Value::Str("--short".into()),
            ],
            None
        )
        .is_ok(),
        "the variadic parameter must absorb every remaining String"
    );

    let path = Value::Str("config.txt".into());
    // The patches: an array, one record, or either of those through the pipe.
    assert!(validate("file_patch", &[path.clone(), patch_array("a", "b")], None).is_ok());
    assert!(
        validate("file_patch", &[path.clone(), patch("a", "b")], None).is_ok(),
        "the single-record form the body reads is refused by its own declaration"
    );
    assert!(validate("file_patch", &[path.clone()], Some(&patch_array("a", "b"))).is_ok());
    assert!(
        validate("file_patch", &[path], Some(&patch("a", "b"))).is_ok(),
        "a single piped record is refused by the declaration"
    );
}

#[test]
fn the_wrong_typed_forms_are_refused_with_a_code() {
    // A non-String program, and a non-String argument after it — which the body
    // used to drop silently. Both stop at validation, so nothing is launched.
    for args in [
        vec![Value::Int(123)],
        vec![Value::Str("git".into()), Value::Int(5)],
    ] {
        let error = call("proc_spawn", args, &mut Env::new())
            .expect_err("a non-String argument must be refused");
        let msg = error.to_string();
        assert!(msg.contains("E_BAD_ARG"), "not coded: {msg}");
        assert!(!msg.contains("E_UNKNOWN"), "{msg}");
        assert!(
            msg.contains("program") || msg.contains("args"),
            "the refusal must name the declared parameter: {msg}"
        );
    }

    // A path that is not a String is refused before anything is opened, null
    // included.
    for path in [Value::Int(123), Value::Null, Value::Array(vec![])] {
        let error = call("file_patch", vec![path], &mut Env::new())
            .expect_err("a non-String path must be refused");
        let msg = error.to_string();
        assert!(msg.contains("E_BAD_ARG"), "not coded: {msg}");
        assert!(msg.contains("path"), "the refusal must name it: {msg}");
    }

    // The patch argument is typed `Any` so that both documented shapes pass
    // validation; a value that is neither is refused by the body, still coded
    // and still before any file is read. The path does not exist, so nothing is
    // created either.
    for bad in [Value::Int(123), Value::Bool(true), Value::Str("x".into())] {
        let error = call(
            "file_patch",
            vec![Value::Str("ae_no_such_private_file.txt".into()), bad],
            &mut Env::new(),
        )
        .expect_err("patches that are neither a record nor an array must be refused");
        assert!(error.to_string().contains("E_BAD_ARG"), "{error}");
    }
}

#[test]
fn the_declared_examples_are_refusals_that_start_and_write_nothing() {
    for name in ["proc_spawn", "file_patch"] {
        let sig = declared(name);
        if sig.params.iter().any(|p| !p.required && !p.variadic) {
            assert!(
                sig.examples.len() >= 2,
                "{name} has an optional parameter, so both call shapes need an example"
            );
        }
        for (code, want) in sig.examples {
            assert_eq!(
                *want, "E_BAD_ARG",
                "the example must claim the validation refusal, never a pid or a record: {code}"
            );
            assert!(
                code.contains("catch") && code.contains(name),
                "the example must catch a call to {name}: {code}"
            );
        }
        // Running them is safe by construction: the first argument is wrong in
        // both, so validation refuses before `guard_dispatch` and before any
        // path is opened.
        assert_eq!(run_examples(name).len(), sig.examples.len());
    }
}

#[test]
fn the_single_record_form_edits_a_private_file() {
    // A real edit on a file this test owns and removes. `tests/write_evidence.rs`
    // covers the array form and the jail; this records the single-record
    // contract the declaration also accepts, positionally and through the pipe.
    let dir = std::env::temp_dir().join(format!("ae_fork_patch_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the private directory");
    let file = dir.join("target.txt");
    std::fs::write(&file, "DEBUG=0\n").expect("seed the private file");
    let path = file.to_string_lossy().into_owned();

    let positional = call(
        "file_patch",
        vec![Value::Str(path.clone()), patch("DEBUG=0", "DEBUG=1")],
        &mut Env::new(),
    );
    // Run the piped form only if the first one was accepted, so a refusal is
    // reported once rather than twice.
    let piped = if positional.is_ok() {
        call_with_input(
            "file_patch",
            vec![Value::Str(path.clone())],
            Some(patch("DEBUG=1", "DEBUG=2")),
            &mut Env::new(),
        )
    } else {
        Ok(Value::Null)
    };

    match (positional, piped) {
        (Ok(Value::Record(first)), Ok(Value::Record(second))) => {
            assert_eq!(first.get("success"), Some(&Value::Bool(true)), "{first:?}");
            assert_eq!(first.get("patches_applied"), Some(&Value::Int(1)));
            assert_eq!(second.get("patches_applied"), Some(&Value::Int(1)));
            assert_eq!(
                std::fs::read_to_string(&file).expect("read the edited file"),
                "DEBUG=2\n",
                "both single-record forms were accepted but the edits are not in the file"
            );
        }
        (Err(e), _) | (_, Err(e)) => {
            // A host enforcing the workspace jail (agent mode, or an explicit
            // AETHER_WORKSPACE) refuses writes outside the workspace, and this
            // private directory is outside it. That refusal comes from the
            // effect gate, which runs after `validate`, so the declared shape
            // was still accepted.
            let msg = e.to_string();
            assert!(
                msg.contains("E_POLICY_DENY") || msg.contains("E_OUTSIDE_WORKSPACE"),
                "the declared shape was refused for a reason that is not the host \
                 policy: {msg}"
            );
            eprintln!("single-record file_patch refused by the host policy: {msg}");
        }
        (Ok(other), _) => panic!("file_patch answered {other:?} instead of a result record"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

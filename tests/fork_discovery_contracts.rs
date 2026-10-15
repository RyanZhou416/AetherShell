//! The discovery contract of the builtins this fork declares or corrected:
//! `sleep`, `sort`, `sort_by` and `json_parse`/`json_stringify`, plus
//! `crypto_hash`, which upstream already declares and is pinned here because an
//! older ontology missed it.
//!
//! `src/signature.rs` serves two jobs from one declaration: the check the
//! dispatcher runs before a body, and the entry an agent reads. These tests pin
//! the second job -- parameter names, types, requiredness, the published tool
//! parameters, and that every advertised form runs. Behaviour is covered
//! elsewhere: `tests/fork_sort_args.rs`, `tests/fork_json_roundtrip.rs`,
//! `tests/sort_by_key.rs`, and `tests/declared_signatures.rs`, which executes
//! every declared example.

use aethershell::agent_api::ontology_describe_json;
use aethershell::builtins::{call_with_input, is_dispatched};
use aethershell::modules::env_with_modules;
use aethershell::signature::{signature_of, Param, Signature, Ty};
use aethershell::value::Value;

/// The declaration for `name`, or a failure naming the defect that is present.
fn declared(name: &str) -> &'static Signature {
    assert!(is_dispatched(name), "{name} is not dispatched at all");
    signature_of(name).unwrap_or_else(|| {
        panic!("{name} has no declaration, so its catalogue entry is derived from its name")
    })
}

/// The declared parameter `name`, or a failure listing what is declared.
fn param<'a>(sig: &'a Signature, name: &str) -> &'a Param {
    sig.params
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| {
            panic!(
                "{} declares no `{name}` parameter; it declares {:?}",
                sig.name,
                sig.params.iter().map(|p| p.name).collect::<Vec<_>>()
            )
        })
}

/// Evaluate `src` against an environment with every module namespace bound.
fn run(src: &str) -> Result<Value, String> {
    let mut env = env_with_modules();
    let program =
        aethershell::parser::parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, &mut env).map_err(|e| e.to_string())
}

fn value(src: &str) -> Value {
    run(src).unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
}

fn ints(values: &[i64]) -> Value {
    Value::Array(values.iter().copied().map(Value::Int).collect())
}

fn strs(values: &[&str]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|s| Value::Str((*s).to_string()))
            .collect(),
    )
}

/// The published tool parameters of `name`, in declaration order:
/// `(name, type, required)`, exactly as an agent reads them.
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

#[test]
fn sleep_declares_seconds_and_returns_null() {
    let sig = declared("sleep");
    assert_eq!(sig.returns, "Null", "sleep returns the null value");
    assert!(sig.subject.is_none(), "sleep has no piped subject");

    let seconds = param(sig, "seconds");
    assert_eq!(seconds.ty, Ty::Numeric, "sleep takes an Int or a Float");
    assert!(
        seconds.required,
        "sleep without a duration is not a call it can honour"
    );
    assert!(
        sig.doc.to_lowercase().contains("second"),
        "the unit must be in the text an agent reads: {}",
        sig.doc
    );

    // The examples run, and they are the cheap end of the range.
    assert_eq!(value("sleep(0)"), Value::Null);
    assert_eq!(value("sleep(0.001)"), Value::Null);

    // The unit is seconds, not milliseconds: 50 ms asked for is at least 50 ms
    // waited. A millisecond reading returns at once and fails the floor.
    let start = std::time::Instant::now();
    assert_eq!(value("sleep(0.05)"), Value::Null);
    let waited = start.elapsed();
    assert!(
        waited >= std::time::Duration::from_millis(40),
        "sleep(0.05) returned after {waited:?}, so the argument is read as milliseconds"
    );

    // A negative duration is refused, with a code an agent can branch on.
    for src in ["sleep(-1)", "sleep(0 - 0.5)"] {
        let err = run(src).expect_err("a negative duration must be refused");
        assert!(err.contains("E_BAD_ARG"), "`{src}` is not coded: {err}");
    }

    // The catalogue entry comes from the declaration rather than the name.
    let def = ontology_describe_json("sleep");
    let signature = def["signature"].as_str().unwrap_or_default();
    assert_ne!(
        signature, "sleep() -> Value",
        "sleep's entry is still derived from its name"
    );
    assert!(
        signature.contains("seconds"),
        "the entry omits the parameter: {signature}"
    );
    assert!(
        signature.ends_with("-> Null"),
        "the entry omits the return type: {signature}"
    );
    assert_eq!(
        published_params("sleep"),
        vec![("seconds".to_string(), "Number".to_string(), true)],
        "the published tool parameters disagree with the declaration"
    );
    assert!(
        def["examples"].as_array().is_some_and(|e| !e.is_empty()),
        "sleep publishes no worked example"
    );
}

#[test]
fn sleep_refuses_non_finite_and_overflowing_durations_without_panicking() {
    for seconds in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        let mut env = env_with_modules();
        let error = call_with_input("sleep", vec![Value::Float(seconds)], None, &mut env)
            .expect_err("an unrepresentable duration must be refused, not panic");
        assert!(error.to_string().contains("E_BAD_ARG"), "{error}");
    }
}

#[test]
fn sort_declares_the_reverse_flag_and_a_pipeline_only_subject() {
    let sig = declared("sort");
    assert_eq!(
        sig.subject,
        Some(Ty::Sequence),
        "sort takes an Array, or a String read as its lines"
    );
    assert!(
        sig.subject_required,
        "`sort` reads its subject from the pipe alone, so the direct form must say so before the body does"
    );
    assert_eq!(sig.returns, "Array");

    let flags = param(sig, "flags");
    assert_eq!(flags.ty, Ty::Str, "the only argument is the flag");
    assert!(!flags.required, "bare `sort` is the common call");
    assert!(
        flags.doc.contains("-r") && flags.doc.contains("--reverse"),
        "the accepted spellings must be named: {}",
        flags.doc
    );
    assert!(
        !sig.params.iter().any(|p| p.ty == Ty::Lambda),
        "`sort` has no comparator parameter; a declaration that accepted one would advertise it"
    );

    // Every form the entry advertises runs.
    assert_eq!(value("[3, 1, 2] | sort"), ints(&[1, 2, 3]));
    assert_eq!(value(r#"[3, 1, 2] | sort("-r")"#), ints(&[3, 2, 1]));
    assert_eq!(value(r#"[3, 1, 2] | sort("--reverse")"#), ints(&[3, 2, 1]));
    assert_eq!(value(r#""b\na" | sort"#), strs(&["a", "b"]));

    // The direct form is refused as a coded condition, not as E_UNKNOWN.
    let mut env = env_with_modules();
    let err = call_with_input("sort", vec![ints(&[3, 1, 2])], None, &mut env)
        .expect_err("sort requires its subject through the pipe");
    let msg = err.to_string();
    assert!(msg.contains("E_BAD_ARG"), "{msg}");
    assert!(!msg.contains("E_UNKNOWN"), "{msg}");

    // The catalogue publishes the declaration, flags included.
    assert_eq!(
        published_params("sort"),
        vec![("flags".to_string(), "String".to_string(), false)],
        "the published tool parameters disagree with the declaration"
    );
}

#[test]
fn sort_by_declares_a_key_and_an_order() {
    let sig = declared("sort_by");
    assert_eq!(sig.subject, Some(Ty::Array));
    assert!(
        !sig.subject_required,
        "sort_by accepts its array positionally as well as through the pipe"
    );
    assert_eq!(sig.returns, "Array");

    let key = param(sig, "key");
    assert_eq!(
        key.ty,
        Ty::Any,
        "the key is a field name or a one-argument lambda, so it cannot be one type"
    );
    assert!(
        !key.required,
        "with no key the builtin's own message names both accepted forms"
    );
    let order = param(sig, "order");
    assert_eq!(order.ty, Ty::Str);
    assert!(!order.required, "ascending is the default");
    assert!(
        order.doc.contains("desc"),
        "the accepted value must be named: {}",
        order.doc
    );

    let first_n = |v: &Value| match v {
        Value::Array(items) => match items.first() {
            Some(Value::Record(m)) => m.get("n").cloned().unwrap_or(Value::Null),
            other => panic!("expected a record first, got {other:?}"),
        },
        other => panic!("expected an array, got {other:?}"),
    };
    let sorted_first = |src: &str| first_n(&value(src));

    // Field name, direction, lambda, and both calling positions.
    assert_eq!(
        sorted_first(r#"[{n: 2}, {n: 1}] | sort_by("n")"#),
        Value::Int(1)
    );
    assert_eq!(
        sorted_first(r#"[{n: 1}, {n: 2}] | sort_by("n", "desc")"#),
        Value::Int(2)
    );
    assert_eq!(
        sorted_first(r#"[{n: 2}, {n: 1}] | sort_by(fn(r) => r.n)"#),
        Value::Int(1)
    );
    assert_eq!(
        sorted_first(r#"[{n: 2}, {n: 1}] | sort_by(fn(r) => r.n, "desc")"#),
        Value::Int(2)
    );
    assert_eq!(
        sorted_first(r#"sort_by([{n: 2}, {n: 1}], "n")"#),
        Value::Int(1)
    );

    assert_eq!(
        published_params("sort_by"),
        vec![
            ("key".to_string(), "Any".to_string(), false),
            ("order".to_string(), "String".to_string(), false),
        ],
        "the published tool parameters disagree with the declaration"
    );
}

#[test]
fn json_parse_and_json_stringify_declare_the_null_contract() {
    let parse = declared("json_parse");
    assert_eq!(
        parse.returns, "Any",
        "a JSON document is any value, null included, so a narrower return type would be wrong"
    );
    assert_eq!(parse.subject, Some(Ty::Str), "the subject is the JSON text");
    assert!(!parse.examples.is_empty());

    let stringify = declared("json_stringify");
    assert_eq!(stringify.returns, "String");
    assert!(!stringify.examples.is_empty());

    // One round trip through the declared pair. The exhaustive set of shapes is
    // run by `tests/fork_json_roundtrip.rs`; this is here because the declared
    // return type of `json_parse` has to admit Null for it to hold.
    assert_eq!(
        value(r#"typeof(json_parse(json_stringify(null)))"#),
        Value::Str("Null".to_string())
    );
    assert_eq!(
        value("json_stringify(null)"),
        Value::Str("null".to_string()),
        "a null must reach JSON as the literal, not as the string \"null\""
    );

    for (name, want) in [("json_parse", "Any"), ("json_stringify", "String")] {
        let def = ontology_describe_json(name);
        let signature = def["signature"].as_str().unwrap_or_default();
        assert_ne!(
            signature,
            format!("{name}() -> Value"),
            "{name}'s entry is still derived from its name"
        );
        assert!(
            signature.ends_with(&format!("-> {want}")),
            "{name}'s entry omits the return type: {signature}"
        );
        assert!(
            def["examples"].as_array().is_some_and(|e| !e.is_empty()),
            "{name} publishes no worked example"
        );
    }
}

#[test]
fn crypto_hash_declares_its_algorithm_parameter() {
    // Declared upstream already; the entry is pinned as a regression because a
    // released ontology did not carry it.
    let sig = declared("crypto_hash");
    assert_eq!(sig.returns, "String");
    assert_eq!(sig.subject, Some(Ty::Str));

    let algorithm = param(sig, "algorithm");
    assert_eq!(algorithm.ty, Ty::Str);
    assert!(!algorithm.required, "sha256 is the documented default");

    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const MD5_ABC: &str = "900150983cd24fb0d6963f7d28e17f72";
    assert_eq!(
        value(r#"crypto_hash("abc")"#),
        Value::Str(SHA256_ABC.to_string())
    );
    assert_eq!(
        value(r#"crypto_hash("abc", "sha256")"#),
        Value::Str(SHA256_ABC.to_string())
    );
    assert_eq!(
        value(r#"crypto_hash("abc", "md5")"#),
        Value::Str(MD5_ABC.to_string())
    );
    assert_eq!(
        value(r#""abc" | crypto_hash"#),
        Value::Str(SHA256_ABC.to_string())
    );
    // In-process and deterministic: same input, same digest, twice.
    assert_eq!(
        value(r#"crypto_hash("abc")"#),
        value(r#"crypto_hash("abc")"#)
    );

    let def = ontology_describe_json("crypto_hash");
    let signature = def["signature"].as_str().unwrap_or_default();
    assert!(
        signature.contains("algorithm"),
        "the entry omits the algorithm parameter: {signature}"
    );
    assert!(
        signature.ends_with("-> String"),
        "the entry omits the return type: {signature}"
    );
}

#[test]
fn the_touched_catalogue_entries_publish_the_declared_metadata() {
    // `definition_from_signature` copies a declaration into the catalogue, so
    // the tool-facing parameters and examples are the declaration's. A second,
    // hand-written description of one builtin is the defect this whole
    // mechanism exists to remove; this pins that these entries do not have one.
    for name in [
        "sleep",
        "sort",
        "sort_by",
        "json_parse",
        "json_stringify",
        "crypto_hash",
    ] {
        let sig = declared(name);
        let def = ontology_describe_json(name);

        assert_eq!(
            def["signature"].as_str(),
            Some(sig.render().as_str()),
            "{name}'s published signature is not the declaration's"
        );

        let published_examples: Vec<&str> = def["examples"]
            .as_array()
            .map(|examples| examples.iter().filter_map(|e| e["code"].as_str()).collect())
            .unwrap_or_default();
        let declared_examples: Vec<&str> = sig.examples.iter().map(|(code, _)| *code).collect();
        assert_eq!(
            published_examples, declared_examples,
            "{name}'s published examples are not the declaration's"
        );

        let declared_params: Vec<(String, String, bool)> = sig
            .params
            .iter()
            .map(|p| (p.name.to_string(), p.ty.as_str().to_string(), p.required))
            .collect();
        assert_eq!(
            published_params(name),
            declared_params,
            "{name}'s published tool parameters are not the declaration's"
        );

        assert!(!sig.doc.is_empty(), "{name} declares no description");
    }
}

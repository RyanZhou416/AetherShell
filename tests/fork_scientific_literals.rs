//! Scientific Float literals and refusal of malformed or unsupported numeric tails.
use aethershell::{
    agent_api::{process_request, AgentRequest},
    modules::env_with_modules,
    parser::{parse_program, ParseError},
    value::Value,
};

fn eval(source: &str) -> Value {
    let program = parse_program(source).unwrap_or_else(|error| panic!("{source}: {error}"));
    aethershell::eval::eval_program(&program, &mut env_with_modules())
        .unwrap_or_else(|error| panic!("{source}: {error}"))
}

fn float(source: &str) -> f64 {
    match eval(source) {
        Value::Float(value) => value,
        other => panic!("{source} must be a Float, got {other:?}"),
    }
}

fn refused(source: &str) -> String {
    let error = parse_program(source).expect_err("invalid numeric text must refuse the program");
    assert!(
        error.downcast_ref::<ParseError>().is_some(),
        "{source}: {error}"
    );
    let message = error.to_string();
    assert!(message.contains("E_PARSE"), "{source}: {message}");
    message
}

#[test]
fn scientific_notation_reaches_the_assignment_as_a_complete_float() {
    assert_eq!(float("x = 1e3; x"), 1000.0);
    assert_eq!(float("let x = 1e3; x"), 1000.0);
    assert_eq!(eval("1e3 == 1000"), Value::Bool(true));
}

#[test]
fn exponent_markers_and_optional_signs_are_supported() {
    for (source, expected) in [
        ("1e3", 1000.0),
        ("1E3", 1000.0),
        ("1e+3", 1000.0),
        ("1E+3", 1000.0),
        ("1e-3", 0.001),
        ("1E-3", 0.001),
        ("12e0", 12.0),
    ] {
        assert_eq!(float(source), expected, "{source}");
    }
}

#[test]
fn a_fraction_and_an_exponent_belong_to_one_literal() {
    for (source, expected) in [
        ("1.25e-2", 0.0125),
        ("1.5E+3", 1500.0),
        ("-2.5e2", -250.0),
        ("1.e2", 100.0),
        ("1.e-2", 0.01),
    ] {
        assert_eq!(float(source), expected, "{source}");
    }
}

#[test]
fn exponent_signs_do_not_consume_later_operators() {
    assert_eq!(float("5-1e2"), -95.0);
    assert_eq!(float("1e-2-3"), 0.01 - 3.0);
    assert_eq!(float("1e+2+3"), 103.0);
    assert_eq!(float("-1e2--2e1"), -80.0);
    assert_eq!(float("2*1.5e2"), 300.0);
}

#[test]
fn scientific_literals_work_in_containers_calls_and_pipelines() {
    assert_eq!(
        eval("[1e2,-2E1,3.5e-1]"),
        Value::Array(vec![
            Value::Float(100.0),
            Value::Float(-20.0),
            Value::Float(0.35)
        ])
    );
    assert_eq!(float("{value: 1e3}.value"), 1000.0);
    assert_eq!(float("abs(-1e3)"), 1000.0);
    assert_eq!(float("[1e2,2E1] | sum"), 120.0);
    assert_eq!(
        eval("[1,2] | map(fn(x) => x*1e2)"),
        Value::Array(vec![Value::Float(100.0), Value::Float(200.0)])
    );
    assert_eq!(float("let f = fn(x) => x+1.0; f(1e3)"), 1001.0);
}

#[test]
fn ordinary_decimals_and_trailing_decimal_points_stay_floats() {
    assert_eq!(float("1.5"), 1.5);
    assert_eq!(float("1."), 1.0);
    assert_eq!(float("-0.5"), -0.5);
    assert_eq!(float("1.5+2.5"), 4.0);
}

#[test]
fn valid_integer_and_si_forms_keep_their_integer_type() {
    for (source, expected) in [
        ("42", 42),
        ("1k", 1000),
        ("1K", 1000),
        ("1M", 1_000_000),
        ("1G", 1_000_000_000),
        ("-2k", -2000),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
    ] {
        assert_eq!(eval(source), Value::Int(expected), "{source}");
    }
    assert_eq!(float("9223372036854775808e0"), 9_223_372_036_854_775_808.0);
}

#[test]
fn negative_zero_retains_its_float_sign() {
    for source in ["-0e0", "-0.0E+5"] {
        let value = float(source);
        assert_eq!(value, 0.0);
        assert!(value.is_sign_negative(), "{source}");
    }
    assert!(float("0e0").is_sign_positive());
}

#[test]
fn finite_extremes_subnormals_and_ieee_underflow_are_supported() {
    assert_eq!(float("1.7976931348623157e308"), f64::MAX);
    assert_eq!(float("5e-324").to_bits(), 1);
    assert_eq!(float("1e-9999"), 0.0);
    assert!(float("-1e-9999").is_sign_negative());
}

#[test]
fn malformed_exponents_are_parse_errors_not_partial_values() {
    for source in [
        "1e",
        "1E",
        "1e+",
        "1e-",
        "1efoo",
        "1e+foo",
        "1e--2",
        "1e+ 2",
        "x = 1e; x",
        "x = 1.5e-; x",
    ] {
        let message = refused(source);
        assert!(
            message.contains("exponent requires at least one digit"),
            "{source}: {message}"
        );
    }
    let message = refused("x = 1e+");
    assert!(message.contains("line 1, column 5"), "{message}");
}

#[test]
fn unsupported_numeric_tails_are_not_separate_identifiers() {
    for literal in [
        "1e3foo", "1e3k", "1e3e4", "1.5k", "1.k", "1.M", "0x10", "1_000", "1key", "1kfoo", "1k2",
        "2Mfoo", "5T", "1e2_3", "1中",
    ] {
        let source = format!("x = {literal}; x");
        let message = refused(&source);
        assert!(message.contains(literal), "{source}: {message}");
        assert!(
            message.contains("unsupported numeric suffix"),
            "{source}: {message}"
        );
    }
}

#[test]
fn overflowing_float_literals_are_refused_instead_of_becoming_infinity() {
    for literal in ["1e309", "-1E+309", "1.7976931348623159e308"] {
        let message = refused(literal);
        assert!(message.contains(literal), "{message}");
        assert!(message.contains("finite 64-bit Float"), "{message}");
    }
    let huge = format!("{}.0", "9".repeat(400));
    assert!(refused(&huge).contains("finite 64-bit Float"));
}

#[test]
fn strings_raw_text_and_identifiers_containing_digits_are_unchanged() {
    assert_eq!(
        eval(r#""1e3 0x10 1_000""#),
        Value::Str("1e3 0x10 1_000".into())
    );
    assert_eq!(
        eval(r#"r"1e3 1e+ ${name}""#),
        Value::Str("1e3 1e+ ${name}".into())
    );
    assert_eq!(eval("let value1e3 = 7; value1e3"), Value::Int(7));
    assert_eq!(eval("len [1,2,3]"), Value::Int(3));
    assert_eq!(eval("let a = 1; let b = 2; a+b"), Value::Int(3));
}

#[test]
fn floating_point_arithmetic_can_still_report_ieee_nonfinite_results() {
    assert!(float("pow(-1.0,0.5)").is_nan());
    assert!(float("1e308*1e308").is_infinite());
    assert_eq!(
        eval("pow(-1.0,0.5) | json_stringify"),
        Value::Str("null".into())
    );
}

#[test]
fn invalid_later_numeric_text_prevents_prior_file_effects() {
    assert_eq!(
        aethershell::safety::current_mode(),
        aethershell::safety::Mode::Human
    );
    assert!(
        std::env::var_os("AETHER_WORKSPACE").is_none(),
        "the positive file control requires no ambient workspace jail"
    );
    let root = tempfile::tempdir().expect("private fixture");
    let path = root.path().join("marker.txt");
    let mut env = env_with_modules();
    env.set_var("marker", Value::Str(path.to_string_lossy().into_owned()))
        .expect("path binding");
    for tail in ["1e+", "1e3foo", "1_000", "1e309"] {
        assert!(
            parse_program(&format!("file_write(marker, \"written\"); x = {tail}; x")).is_err(),
            "{tail}"
        );
        assert!(!path.exists(), "{tail}");
    }
    let program =
        parse_program("file_write(marker, \"written\"); x = 1e3; x").expect("positive control");
    assert_eq!(
        aethershell::eval::eval_program(&program, &mut env).expect("control writes"),
        Value::Float(1000.0)
    );
    assert_eq!(std::fs::read(&path).expect("actual content"), b"written");
}

#[test]
fn float_discovery_examples_are_real_float_expressions() {
    let ontology = aethershell::agent_api::build_language_ontology();
    let kind = ontology
        .types
        .iter()
        .find(|kind| kind.name == "Float")
        .expect("Float metadata");
    assert!(kind.description.contains("e/E exponent"));
    assert!(kind.description.contains("E_PARSE"));
    assert!(!kind.examples.is_empty());
    for source in &kind.examples {
        assert!(float(source).is_finite(), "{source}");
    }
}

#[test]
fn agent_api_reports_scientific_results_and_refuses_invalid_forms() {
    let response = process_request(&AgentRequest::Eval {
        code: "x = 1e3; x".into(),
    });
    assert!(response.success, "{:?}", response.error);
    assert_eq!(response.result_type.as_deref(), Some("Float"));
    assert_eq!(response.result.expect("result").as_f64(), Some(1000.0));
    for code in ["x = 1e; x", "x = 1e3foo; x", "x = 1e309; x"] {
        let response = process_request(&AgentRequest::Eval { code: code.into() });
        assert!(!response.success, "{code}");
        assert!(
            response.error.expect("failure").contains("E_PARSE"),
            "{code}"
        );
        assert!(response.result.is_none(), "{code}");
    }
}

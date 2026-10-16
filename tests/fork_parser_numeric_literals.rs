//! Parser numeric literals: the signed 64-bit domain of a source literal, the
//! SI suffix's checked scale, and `-` as an operator wherever an expression can
//! end.
//!
//! Three defects are covered. An integer literal past `i64` reached the
//! evaluator as `0`; an SI form past `i64` saturated to the boundary; and a `-`
//! immediately followed by a digit was always the sign of a literal, so `5-3`
//! was two statements and `x = 5-3` assigned `5`. The first two now refuse the
//! whole literal with `E_PARSE` before anything runs; the third folds a sign
//! only where no expression can end.

use aethershell::agent_api::{process_request, AgentRequest};
use aethershell::modules::env_with_modules;
use aethershell::safety::{current_mode, Mode};
use aethershell::value::Value;

/// The failure text of a source the parser must refuse.
fn parse_failure(src: &str) -> String {
    match aethershell::parser::parse_program(src) {
        Ok(stmts) => panic!(
            "`{src}` parsed into {} statement(s) instead of failing",
            stmts.len()
        ),
        Err(error) => error.to_string(),
    }
}

/// The same refusal, asserted to be the coded parse error.
fn parse_refusal(src: &str) -> String {
    let text = parse_failure(src);
    assert!(
        text.contains("E_PARSE"),
        "`{src}` did not carry E_PARSE: {text}"
    );
    text
}

fn eval(src: &str) -> Value {
    let mut env = env_with_modules();
    let program =
        aethershell::parser::parse_program(src).unwrap_or_else(|e| panic!("parse `{src}`: {e}"));
    aethershell::eval::eval_program(&program, &mut env)
        .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
}

#[test]
fn the_signed_boundaries_stay_exact() {
    assert_eq!(eval("9223372036854775807"), Value::Int(i64::MAX));
    assert_eq!(eval("-9223372036854775808"), Value::Int(i64::MIN));
    assert_eq!(
        eval("(-9223372036854775808)"),
        Value::Int(i64::MIN),
        "a parenthesized minimum is still one literal"
    );
    assert_eq!(
        eval("[9223372036854775807, -9223372036854775808]"),
        Value::Array(vec![Value::Int(i64::MAX), Value::Int(i64::MIN)])
    );
}

#[test]
fn an_out_of_range_integer_literal_is_refused_not_zero() {
    for literal in [
        "9223372036854775808",
        "18446744073709551615",
        "99999999999999999999",
        "-9223372036854775809",
    ] {
        let text = parse_refusal(literal);
        assert!(
            text.contains(literal),
            "the refusal of `{literal}` does not name it: {text}"
        );
    }
    // The message carries the literal's own opening position.
    let text = parse_refusal("x = 9223372036854775808");
    assert!(text.contains("line 1, column 5"), "{text}");
}

#[test]
fn an_out_of_range_pattern_literal_cannot_match_zero() {
    let text = parse_refusal(r#"match 0 { 9223372036854775808 => "zero", _ => "other" }"#);
    assert!(text.contains("9223372036854775808"), "{text}");

    // The boundary literals still match themselves, and a valid SI pattern
    // still scales.
    assert_eq!(
        eval(r#"match 9223372036854775807 { 9223372036854775807 => "max", _ => "other" }"#),
        Value::Str("max".into())
    );
    assert_eq!(
        eval(r#"match -9223372036854775808 { -9223372036854775808 => "min", _ => "other" }"#),
        Value::Str("min".into())
    );
    assert_eq!(
        eval(r#"match 1000 { 1k => "grand", _ => "other" }"#),
        Value::Str("grand".into())
    );
}

#[test]
fn a_valid_si_suffix_is_exact_including_zero() {
    assert_eq!(eval("0k"), Value::Int(0));
    assert_eq!(eval("1k"), Value::Int(1_000));
    assert_eq!(eval("1K"), Value::Int(1_000));
    assert_eq!(eval("1M"), Value::Int(1_000_000));
    assert_eq!(eval("1G"), Value::Int(1_000_000_000));
    assert_eq!(
        eval("9223372036854775k"),
        Value::Int(9_223_372_036_854_775_000)
    );
    assert_eq!(
        eval("(-9223372036854775k)"),
        Value::Int(-9_223_372_036_854_775_000)
    );
    // Without a suffix the same digits are a plain literal.
    assert_eq!(eval("9223372036854775"), Value::Int(9_223_372_036_854_775));
}

#[test]
fn an_si_form_past_i64_is_refused_with_its_whole_lexeme() {
    for (src, lexeme) in [
        ("9223372036854776k", "9223372036854776k"),
        ("9223372036854775807G", "9223372036854775807G"),
        ("99999999999999999999k", "99999999999999999999k"),
        ("(-9223372036854776k)", "-9223372036854776k"),
    ] {
        let text = parse_refusal(src);
        assert!(
            text.contains(lexeme),
            "`{src}` does not name `{lexeme}`: {text}"
        );
    }
    // The suffix is not left behind as an identifier: the refusal names the
    // digits and the suffix, at the literal's own column.
    let text = parse_refusal("x = 9223372036854776k");
    assert!(text.contains("9223372036854776k"), "{text}");
    assert!(text.contains("line 1, column 5"), "{text}");
}

#[test]
fn an_si_literal_inside_an_argument_is_checked() {
    assert_eq!(eval("len([1G, 2M, 3k])"), Value::Int(3));
    assert!(parse_refusal("len([9223372036854776k])").contains("9223372036854776k"));
}

#[test]
fn a_minus_after_an_expression_is_subtraction_in_every_spacing() {
    assert_eq!(eval("5-3"), Value::Int(2));
    assert_eq!(eval("1-1"), Value::Int(0));
    assert_eq!(eval("2*3-1"), Value::Int(5));
    assert_eq!(eval("1- 1"), Value::Int(0));
    assert_eq!(eval("1 -1"), Value::Int(0));
    assert_eq!(eval("1 - 1"), Value::Int(0));
    assert_eq!(eval("1-1 == 0"), Value::Bool(true));
}

#[test]
fn subtraction_reaches_assignments_calls_and_containers() {
    assert_eq!(eval("x = 5-3; x"), Value::Int(2));
    assert_eq!(eval("x = 2*3-1; x"), Value::Int(5));
    assert_eq!(eval("let x = 5; x-1"), Value::Int(4));
    assert_eq!(eval("len([1,2,3])-1"), Value::Int(2));
    assert_eq!(
        eval("[-1, 2-1]"),
        Value::Array(vec![Value::Int(-1), Value::Int(1)])
    );
    assert_eq!(eval("{a: 0-1, b: 2-1}"), eval("{a: -1, b: 1}"));
    // A variable name that needs no normalization beyond its own spelling.
    assert_eq!(eval("x_1 = -2; x_1-1"), Value::Int(-3));
}

#[test]
fn a_signed_literal_still_starts_an_operand() {
    assert_eq!(eval("-5"), Value::Int(-5));
    assert_eq!(eval("-(1+2)"), Value::Int(-3));
    assert_eq!(eval("x = 1; -x"), Value::Int(-1));
    assert_eq!(eval("-5--3"), Value::Int(-2));
    assert_eq!(eval("5--3"), Value::Int(8));
    assert_eq!(eval("abs(-3)"), Value::Int(3));
    assert_eq!(eval("min([-9223372036854775808])"), Value::Int(i64::MIN));
    assert_eq!(
        eval("str(-9223372036854775808)"),
        Value::Str("-9223372036854775808".into())
    );
}

#[test]
fn a_word_call_with_a_positive_argument_is_unchanged() {
    assert_eq!(eval("len [1,2,3]"), Value::Int(3));
}

#[test]
fn float_literals_are_unchanged() {
    assert_eq!(eval("1.5"), Value::Float(1.5));
    assert_eq!(eval("1.5 + 2.5"), Value::Float(4.0));
}

#[test]
fn an_out_of_range_literal_in_a_later_statement_refuses_the_program() {
    let text = parse_refusal("x = 1\n9223372036854775808");
    assert!(text.contains("9223372036854775808"), "{text}");
    let text = parse_refusal("x = 1\nx = 9223372036854775808");
    assert!(text.contains("9223372036854775808"), "{text}");
}

#[test]
fn a_refused_literal_runs_no_earlier_statement() {
    assert_eq!(
        current_mode(),
        Mode::Human,
        "run this without AETHER_MODE, AETHER_AGENT or AETHER_WORKSPACE set"
    );
    assert!(
        std::env::var_os("AETHER_WORKSPACE").is_none(),
        "the private file-effect control requires AETHER_WORKSPACE to be unset"
    );
    let dir = tempfile::tempdir().expect("scratch directory");
    let marker = dir.path().join("marker.txt");

    let mut env = env_with_modules();
    env.set_var("marker", Value::Str(marker.to_string_lossy().into_owned()))
        .expect("bind the marker path");

    let refused = "file_write(marker, \"written\")\n9223372036854775808";
    assert!(
        aethershell::parser::parse_program(refused).is_err(),
        "the out-of-range literal must refuse the program"
    );
    assert!(
        !marker.exists(),
        "a refused program ran its earlier statement"
    );

    let control = "file_write(marker, \"written\")\n9223372036854775807";
    let program = aethershell::parser::parse_program(control).expect("the control parses");
    aethershell::eval::eval_program(&program, &mut env).expect("the control runs");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("the control wrote the marker"),
        "written"
    );
}

#[test]
fn the_agent_api_reports_a_refused_literal_as_parse() {
    for code in ["9223372036854775808", "9223372036854776k"] {
        let response = process_request(&AgentRequest::Eval {
            code: code.to_string(),
        });
        assert!(
            !response.success,
            "`{code}` answered: {:?}",
            response.result
        );
        let error = response
            .error
            .unwrap_or_else(|| panic!("`{code}` carries no error text"));
        assert!(error.contains("E_PARSE"), "`{code}`: {error}");
        assert!(error.contains(code), "`{code}` is not named: {error}");
    }
}

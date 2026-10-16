//! Checked native integer arithmetic, Float-to-Int bounds and range budgets.
//! Tests use native i64/f64 values rather than the DSH adapter's JSON-number range.

use aethershell::builtins::{call, call_with_input};
use aethershell::env::Env;
use aethershell::eval::eval_program;
use aethershell::numeric::{checked_pow_i64, float_to_i64, range_len};
use aethershell::parser::parse_program;
use aethershell::safety::SafetyError;
use aethershell::value::Value;

fn eval(src: &str) -> anyhow::Result<Value> {
    let program = parse_program(src).unwrap_or_else(|e| panic!("parse {src}: {e:?}"));
    eval_program(&program, &mut Env::new())
}

fn refusal(src: &str) -> anyhow::Error {
    match eval(src) {
        Ok(v) => panic!("{src} returned {v:?}; expected a refusal"),
        Err(e) => e,
    }
}

/// The failure carries the expected stable code and names its cause.
fn assert_refused(error: &anyhow::Error, code: &str, needle: &str) {
    let safety = error
        .downcast_ref::<SafetyError>()
        .unwrap_or_else(|| panic!("uncoded failure: {error}"));
    assert_eq!(safety.code.as_str(), code, "wrong code for: {error}");
    assert!(
        safety.message.contains(needle),
        "`{needle}` missing from the refusal: {}",
        safety.message
    );
}

fn run(name: &str, args: Vec<Value>) -> anyhow::Result<Value> {
    call(name, args, &mut Env::new())
}

fn piped(name: &str, input: Value, args: Vec<Value>) -> anyhow::Result<Value> {
    call_with_input(name, args, Some(input), &mut Env::new())
}

fn i(n: i64) -> Value {
    Value::Int(n)
}

fn f(x: f64) -> Value {
    Value::Float(x)
}

fn array(items: Vec<Value>) -> Value {
    Value::Array(items)
}

fn ints(xs: &[i64]) -> Value {
    array(xs.iter().map(|n| Value::Int(*n)).collect())
}

fn array_len(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.len(),
        other => panic!("expected an Array, got {other:?}"),
    }
}

// ── Int arithmetic is checked, not wrapped ──────────────────────────────

#[test]
fn integer_overflow_is_refused() {
    assert_refused(&refusal("9223372036854775807 + 1"), "E_BAD_ARG", "overflow");
    assert_refused(
        &refusal("(0 - 9223372036854775807 - 1) - 1"),
        "E_BAD_ARG",
        "overflow",
    );
    assert_refused(&refusal("9223372036854775807 * 2"), "E_BAD_ARG", "overflow");
    assert_refused(
        &refusal("(0 - 9223372036854775807 - 1) * (0 - 1)"),
        "E_BAD_ARG",
        "overflow",
    );
    // Unary `-` on the computed minimum, not the literal the lexer pre-signs.
    assert_refused(
        &refusal("-(0 - 9223372036854775807 - 1)"),
        "E_BAD_ARG",
        "negation",
    );
}

#[test]
fn i64_min_remainder_by_minus_one_is_exact_zero() {
    // The exact remainder is representable; plain `%` panicked here, exit 101.
    assert_eq!(
        eval("(0 - 9223372036854775807 - 1) % (0 - 1)").expect("MIN % -1"),
        i(0)
    );
    assert!(eval("(0 - 9223372036854775807 - 1) % 3").is_ok());
}

#[test]
fn ordinary_integer_arithmetic_is_unchanged() {
    assert_eq!(
        eval("9223372036854775806 + 1").expect("MAX - 1 + 1"),
        i(i64::MAX)
    );
    assert_eq!(eval("1 + 1").expect("1 + 1"), i(2));
    assert_eq!(eval("7 - 9").expect("7 - 9"), i(-2));
    assert_eq!(eval("6 * 7").expect("6 * 7"), i(42));
    assert_eq!(eval("7 % 3").expect("7 % 3"), i(1));
    assert_eq!(eval("(0 - 7) % 3").expect("truncated remainder"), i(-1));
    assert_eq!(eval("-(0 - 5)").expect("negation"), i(5));
    assert!(
        eval("1 % 0").is_err(),
        "a zero divisor must still be refused"
    );
}

// ── Positive integer power is exact ─────────────────────────────────────

#[test]
fn integer_power_is_exact_within_the_bound() {
    // A `u32` cast used to answer a different question for these exponents.
    assert_eq!(eval("1 ^ 4294967296").expect("1 ^ 2^32"), i(1));
    assert_eq!(eval("(0 - 1) ^ 4294967297").expect("(-1) ^ odd"), i(-1));
    assert_eq!(eval("0 ^ 4294967296").expect("0 ^ 2^32"), i(0));
    assert_eq!(eval("2 ^ 10").expect("2 ^ 10"), i(1024));
    assert_eq!(
        eval("2 ^ 62").expect("2 ^ 62"),
        i(4_611_686_018_427_387_904)
    );
    assert_eq!(eval("(0 - 2) ^ 63").expect("(-2) ^ 63"), i(i64::MIN));
    assert_eq!(
        eval("(0 - 9223372036854775807 - 1) ^ 1").expect("MIN ^ 1"),
        i(i64::MIN)
    );
}

#[test]
fn integer_power_beyond_the_bound_is_refused() {
    for src in ["2 ^ 63", "2 ^ 64", "3 ^ 40"] {
        assert_refused(&refusal(src), "E_BAD_ARG", "overflow");
    }
}

// ── builtin pow shares the exact helper ─────────────────────────────────

#[test]
fn builtin_pow_is_exact_within_range_and_refuses_beyond() {
    assert_eq!(
        run("pow", vec![i(2), i(62)]).expect("pow(2, 62)"),
        i(4_611_686_018_427_387_904)
    );
    assert_eq!(
        run("pow", vec![i(i64::MAX), i(1)]).expect("pow"),
        i(i64::MAX)
    );
    assert_eq!(
        run("pow", vec![i(i64::MIN), i(1)]).expect("pow"),
        i(i64::MIN)
    );
    let e = run("pow", vec![i(2), i(64)]).expect_err("pow(2, 64) must refuse");
    assert_refused(&e, "E_BAD_ARG", "overflow");
}

#[test]
fn builtin_pow_accepts_a_piped_base() {
    // The declared form is a Numeric subject plus an exponent: `2 | pow(3)`.
    assert_eq!(piped("pow", i(2), vec![i(3)]).expect("2 | pow(3)"), i(8));
}

#[test]
fn builtin_pow_keeps_float_and_negative_exponent_semantics() {
    match run("pow", vec![f(-1.0), f(0.5)]).expect("pow(-1.0, 0.5)") {
        Value::Float(x) => assert!(x.is_nan(), "expected NaN, got {x}"),
        other => panic!("expected a NaN Float, got {other:?}"),
    }
    assert_eq!(run("pow", vec![i(2), i(-1)]).expect("pow(2, -1)"), f(0.5));
    assert_eq!(run("pow", vec![i(1), i(-5)]).expect("pow(1, -5)"), i(1));
}

// ── abs and the rounding builtins ───────────────────────────────────────

#[test]
fn abs_of_the_integer_minimum_is_refused() {
    let e = run("abs", vec![i(i64::MIN)]).expect_err("abs(i64::MIN) must refuse");
    assert_refused(&e, "E_BAD_ARG", "abs");
    assert_eq!(run("abs", vec![i(-5)]).expect("abs(-5)"), i(5));
}

#[test]
fn floor_and_ceil_refuse_non_finite_and_out_of_range() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300] {
        let e = run("floor", vec![f(bad)]).expect_err("floor must refuse");
        assert_refused(&e, "E_BAD_ARG", "floor");
        let e = run("ceil", vec![f(bad)]).expect_err("ceil must refuse");
        assert_refused(&e, "E_BAD_ARG", "ceil");
    }
}

#[test]
fn floor_ceil_and_round_accept_the_lower_boundary() {
    // -2^63 is representable; only 2^63 is out of range.
    for name in ["floor", "ceil", "round"] {
        assert_eq!(
            run(name, vec![f(-9_223_372_036_854_775_808.0)]).expect(name),
            i(i64::MIN),
            "{name} rejected the lower boundary"
        );
    }
}

#[test]
fn round_refuses_non_finite_and_out_of_range() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300] {
        let e = run("round", vec![f(bad)]).expect_err("round must refuse");
        assert_refused(&e, "E_BAD_ARG", "round");
        if !bad.is_finite() {
            let e = run("round", vec![f(bad), i(2)]).expect_err("non-finite input must refuse");
            assert_refused(&e, "E_BAD_ARG", "finite");
        }
    }
    let Value::Float(large) = run("round", vec![f(1e300), i(2)]).expect("finite scaled value")
    else {
        panic!("positive digits must return a Float")
    };
    assert!(large.is_finite() && large > 1e299);
}

#[test]
fn round_honours_digits_and_refuses_non_finite_scaling() {
    assert_eq!(
        run("round", vec![f(4.966_101_694_915_254), i(2)]).expect("round(x, 2)"),
        f(4.97)
    );
    assert_eq!(run("round", vec![f(4.5)]).expect("round(4.5)"), i(5));
    // A finite input whose scaling overflows to infinity is refused, not
    // rendered as a silent JSON null.
    let e = run("round", vec![f(1e308), i(2)]).expect_err("round(1e308, 2)");
    assert_refused(&e, "E_BAD_ARG", "finite");
}

// ── sum and avg ─────────────────────────────────────────────────────────

#[test]
fn sum_is_exact_or_refused() {
    let e = run("sum", vec![ints(&[i64::MAX, 1])]).expect_err("sum overflow");
    assert_refused(&e, "E_BAD_ARG", "overflow");
    let e = run("sum", vec![ints(&[i64::MIN, -1])]).expect_err("sum overflow");
    assert_refused(&e, "E_BAD_ARG", "overflow");
    // Cancellation is exact, and a later Float never inherits a wrapped sign.
    assert_eq!(
        run("sum", vec![ints(&[i64::MAX, 1, -1])]).expect("cancelling sum"),
        i(i64::MAX)
    );
    match run("sum", vec![array(vec![i(i64::MAX), i(1), f(1.5)])]).expect("mixed sum") {
        Value::Float(x) => assert!(x > 0.0, "the Float total kept the wrapped sign: {x}"),
        other => panic!("expected a Float total, got {other:?}"),
    }
    assert_eq!(run("sum", vec![array(vec![])]).expect("sum([])"), i(0));
}

#[test]
fn avg_of_an_empty_array_is_refused_like_min_and_max() {
    for name in ["avg", "mean", "min", "max"] {
        let e = run(name, vec![array(vec![])]).expect_err("empty aggregate must refuse");
        assert_refused(&e, "E_BAD_ARG", "empty");
    }
    assert_eq!(run("avg", vec![ints(&[2, 4, 6, 8])]).expect("avg"), f(5.0));
}

// ── range counts before allocating ──────────────────────────────────────

#[test]
fn range_len_counts_without_allocating() {
    // The 1,000,000 boundary is returned as a count, with no array built.
    assert_eq!(range_len(0, 1_000_000, 1), Some(1_000_000));
    assert_eq!(range_len(0, 1_000_001, 1), None);
    assert_eq!(range_len(0, 3, 0), None, "a zero step has no count");
    assert_eq!(range_len(3, 0, 1), Some(0), "a wrong-way range is empty");
    assert_eq!(range_len(5, 0, -2), Some(3));
    assert_eq!(range_len(i64::MIN, i64::MAX, 1), None);
    // The overflow witness: one item, decided in i128 before any allocation.
    assert_eq!(range_len(9_223_372_036_854_775_806, i64::MAX, 3), Some(1));
}

#[test]
fn numeric_helpers_hold_their_boundaries() {
    assert_eq!(float_to_i64(-9_223_372_036_854_775_808.0), Some(i64::MIN));
    assert_eq!(
        float_to_i64(9_223_372_036_854_775_808.0),
        None,
        "2^63 is one past the largest i64 and must be refused"
    );
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e300] {
        assert_eq!(float_to_i64(bad), None, "{bad} is not an i64");
    }
    assert_eq!(float_to_i64(2.0), Some(2));

    assert_eq!(checked_pow_i64(2, 62), Some(4_611_686_018_427_387_904));
    assert_eq!(checked_pow_i64(2, 63), None);
    assert_eq!(checked_pow_i64(-2, 63), Some(i64::MIN));
    assert_eq!(checked_pow_i64(1, u64::MAX), Some(1));
    assert_eq!(checked_pow_i64(-1, u64::MAX), Some(-1));
    assert_eq!(checked_pow_i64(0, u64::MAX), Some(0));
}

#[test]
fn range_budget_boundary_and_refusal() {
    let full = run("range", vec![i(0), i(1_000_000)]).expect("range(0, 1_000_000)");
    assert_eq!(array_len(&full), 1_000_000);
    let e = run("range", vec![i(0), i(1_000_001)]).expect_err("over budget");
    assert_refused(&e, "E_BUDGET_EXCEEDED", "range would produce");
    // A billion-item range is refused from the count alone.
    let e = run("range", vec![i(5_000_000_000)]).expect_err("over budget");
    assert_refused(&e, "E_BUDGET_EXCEEDED", "range would produce");
}

#[test]
fn range_ordinary_empty_and_overflow_witness_terminate() {
    assert_eq!(
        run("range", vec![i(0), i(3)]).expect("range(0, 3)"),
        ints(&[0, 1, 2])
    );
    assert_eq!(
        run("range", vec![i(2), i(7)]).expect("range(2, 7)"),
        ints(&[2, 3, 4, 5, 6])
    );
    assert_eq!(
        run("range", vec![i(0), i(10), i(2)]).expect("range(0, 10, 2)"),
        ints(&[0, 2, 4, 6, 8])
    );
    assert_eq!(
        run("range", vec![i(5), i(0), i(-2)]).expect("range(5, 0, -2)"),
        ints(&[5, 3, 1])
    );
    assert_eq!(
        run("range", vec![i(3), i(0)]).expect("empty range"),
        array(vec![])
    );
    let e = run("range", vec![i(0), i(5), i(0)]).expect_err("zero step");
    assert_refused(&e, "E_BAD_ARG", "step");
    // `current += step` wrapped here before the fix and never terminated.
    assert_eq!(
        run(
            "range",
            vec![i(9_223_372_036_854_775_806), i(i64::MAX), i(3)]
        )
        .expect("range(MAX-1, MAX, 3)"),
        ints(&[9_223_372_036_854_775_806])
    );
}

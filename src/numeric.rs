//! Shared checked integer powers, Float-to-Int bounds and materialized range limits.

/// The largest array `range` will build, in items.
///
/// A fixed guard chosen here rather than read from the environment: it bounds
/// one value's size without becoming a deployment knob. The item count is
/// computed before the allocation, so an oversized range is refused instead of
/// attempted.
pub(crate) const MAX_RANGE_ITEMS: usize = 1_000_000;

/// `base` raised to `exp` by squaring, or `None` when the exact result does not
/// fit in `i64`.
///
/// `exp` is unsigned and unbounded: `0`, `1` and `-1` are exact for every
/// exponent, so `1 ^ 4294967296` answers `1` where the old `as u32` cast asked
/// a different question. The running product is never widened to `f64`, so a
/// result that fits is bit-exact.
pub fn checked_pow_i64(base: i64, exp: u64) -> Option<i64> {
    let mut result: i64 = 1;
    let mut square = base;
    let mut e = exp;
    while e > 0 {
        if e & 1 == 1 {
            result = result.checked_mul(square)?;
        }
        e >>= 1;
        // Square only while another bit remains: squaring past the last bit can
        // overflow a factor the result never multiplies.
        if e > 0 {
            square = square.checked_mul(square)?;
        }
    }
    Some(result)
}

/// `f` as an `i64`, or `None` when it is not finite or not in range.
///
/// The upper bound is exclusive. `2^63` has no `i64` value, and a saturating
/// `as` cast answered `i64::MAX` for it; `-2^63` is representable and accepted.
pub fn float_to_i64(f: f64) -> Option<i64> {
    /// `-2^63`, the smallest representable `i64`.
    const LOWER: f64 = -9_223_372_036_854_775_808.0;
    /// `2^63`, one past the largest representable `i64`.
    const UPPER: f64 = 9_223_372_036_854_775_808.0;
    if f.is_finite() && f >= LOWER && f < UPPER {
        Some(f as i64)
    } else {
        None
    }
}

/// How many items `start..end` by `step` produces, or `None` when that count
/// exceeds `MAX_RANGE_ITEMS` or `step` is zero.
///
/// The count is computed in `i128` before the caller allocates anything, so a
/// step whose addition would overflow `i64` ends the range at its true last
/// item instead of wrapping the loop, and a range past the budget is refused
/// rather than attempted. `Some(0)` is an empty range: a step that moves away
/// from the bound produces nothing.
pub fn range_len(start: i64, end: i64, step: i64) -> Option<usize> {
    if step == 0 {
        return None;
    }
    let forward = step > 0;
    if (forward && start >= end) || (!forward && start <= end) {
        return Some(0);
    }
    // `i128` keeps both the distance and `|step|` exact, including `i64::MIN`.
    let distance = (end as i128 - start as i128).abs();
    let stride = (step as i128).abs();
    let count = (distance - 1) / stride + 1;
    if count > MAX_RANGE_ITEMS as i128 {
        return None;
    }
    Some(count as usize)
}

//! Byte-budgeted UTF-8 text prefixes.
//!
//! Logging and title paths cap strings by *bytes*, and slicing a `String` at a
//! byte index panics when that index lands inside a multi-byte character. This
//! module owns the one floor-to-boundary rule those callers share.

/// The longest prefix of `s` that is at most `max_bytes` bytes and ends on a
/// UTF-8 character boundary.
///
/// The budget is bytes, not characters, so a caller's existing byte bound is
/// preserved: the result is never longer than `max_bytes`, and it is
/// byte-identical to `&s[..max_bytes]` whenever that index already falls on a
/// boundary. The floor loop runs at most three times, because one UTF-8
/// character is at most four bytes.
///
/// ```
/// use aethershell::text::prefix_bytes;
/// assert_eq!(prefix_bytes("abcdef", 3), "abc"); // ASCII: unchanged
/// assert_eq!(prefix_bytes("你好", 4), "你"); // floor 4 to the 3-byte boundary
/// assert_eq!(prefix_bytes("你好", 6), "你好");
/// ```
pub fn prefix_bytes(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_cuts_are_byte_identical_to_slicing() {
        assert_eq!(prefix_bytes("abcdef", 0), "");
        assert_eq!(prefix_bytes("abcdef", 2), "ab");
        assert_eq!(prefix_bytes("abcdef", 6), "abcdef");
        assert_eq!(prefix_bytes("abcdef", 9), "abcdef");
    }

    #[test]
    fn multibyte_cuts_floor_to_a_character_boundary() {
        // CJK is three bytes: byte 4 is inside the first character.
        assert_eq!(prefix_bytes("中中", 4), "中");
        // Emoji is four bytes: byte 6 is inside the second character.
        assert_eq!(prefix_bytes("😀😀", 6), "😀");
        assert_eq!(prefix_bytes("a中b", 2), "a");
    }

    #[test]
    fn every_budget_stays_within_budget_on_a_boundary() {
        let s = "a中😀é中";
        for budget in 0..=s.len() + 2 {
            let cut = prefix_bytes(s, budget);
            assert!(cut.len() <= budget.min(s.len()));
            assert!(s.is_char_boundary(cut.len()));
            assert!(s.starts_with(cut));
        }
    }

    #[test]
    fn unchanged_input_is_borrowed_not_rebuilt() {
        let owned = String::from("中");
        assert_eq!(prefix_bytes(&owned, 0), "");
        assert_eq!(prefix_bytes(&owned, 3), "中");
        assert!(std::ptr::eq(prefix_bytes(&owned, 3), owned.as_str()));
    }

    #[test]
    fn usize_max_returns_the_whole_string() {
        let s = "中";
        assert_eq!(prefix_bytes(s, usize::MAX), s);
    }
}

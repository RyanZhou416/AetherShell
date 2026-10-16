//! JSON text ingestion requires signed-i64 integer lexemes before native conversion.
//! Decimal and exponent forms retain Float semantics; quoted values and keys are
//! skipped. Syntax and Float-range errors remain the JSON parser's responsibility.

/// The first JSON integer lexeme in `text` whose value does not fit `i64`.
///
/// Returns `Ok(())` when every integer lexeme fits, or the borrowed lexeme that
/// did not. The caller turns that lexeme into its own error, because only the
/// caller knows which builtin is answering.
///
/// `text` MUST already be syntax-validated JSON — `serde_json::from_str`
/// accepted it. The scan relies on that: a `"` always opens a string that ends,
/// a `\` inside one is always followed by the escaped character, and a `-` or
/// digit outside a string always opens a number lexeme. Reading text that was
/// never validated would be a second, weaker JSON parser.
pub(crate) fn validate_integer_literals(text: &str) -> Result<(), &str> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // A quoted string is skipped whole, so a number written inside one
            // (a string value or an object key) is never read as a lexeme.
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() && is_number_byte(bytes[i]) {
                    i += 1;
                }
                // `start` and `i` both sit on ASCII bytes, so this is a char
                // boundary even when the text around the lexeme is not ASCII.
                let literal = &text[start..i];
                if !literal.bytes().any(|b| matches!(b, b'.' | b'e' | b'E'))
                    && literal.parse::<i64>().is_err()
                {
                    return Err(literal);
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// One byte of a JSON number lexeme, per the grammar serde already accepted.
fn is_number_byte(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
}

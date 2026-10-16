//! The `${...}` template scanner the native and WASM evaluators share.
//!
//! A hole ends at its own `}`, not at the first `}` after a `{`: a brace inside
//! a quoted string, a raw literal or a comment is text. Every hole is parsed
//! before any is evaluated, so refusal withholds this literal's holes only.

use anyhow::Result;

use crate::ast::{Expr, Stmt};
use crate::parser::{parse_program, ParseError, MAX_RAW_HASHES};

/// The characters that open a hole.
const HOLE_OPEN: &str = "${";

/// One piece of an interpolated string literal.
#[derive(Debug)]
pub enum Segment<'a> {
    /// Text between holes, carried into the result unchanged.
    Literal(&'a str),
    /// The single expression inside one `${...}`, parsed once.
    Expression(Expr),
}

/// A string literal split into its literal runs and its holes.
#[derive(Debug)]
pub struct Template<'a> {
    /// The pieces in source order.
    pub segments: Vec<Segment<'a>>,
}

/// Split a string literal's `source` into literal runs and parsed holes.
///
/// The pieces borrow from `source`. Every hole is parsed here, before the
/// caller evaluates anything, so one hole that does not parse refuses the whole
/// literal. A hole holding no expression, more than one statement, or a
/// statement that is not an expression is a [`ParseError`] (`E_PARSE`), as is a
/// `${` with no matching `}`. Withholding the literal's holes does not undo a
/// statement the program already ran.
///
/// # Errors
/// `E_PARSE` when a hole is unterminated, empty, or not exactly one expression;
/// otherwise the parser's own failure for the hole's text.
pub fn parse_template(source: &str) -> Result<Template<'_>> {
    let mut segments = Vec::new();
    let mut cursor = 0usize;
    let mut scan = 0usize;
    let mut hole = 0usize;

    while let Some(offset) = source[scan..].find(HOLE_OPEN) {
        let open = scan + offset;
        let body = open + HOLE_OPEN.len();
        hole += 1;
        let close = hole_end(source, body, hole)?;
        if open > cursor {
            segments.push(Segment::Literal(&source[cursor..open]));
        }
        segments.push(Segment::Expression(parse_hole(&source[body..close], hole)?));
        cursor = close + 1;
        scan = cursor;
    }

    if cursor < source.len() {
        segments.push(Segment::Literal(&source[cursor..]));
    }
    Ok(Template { segments })
}

/// Parse the text of hole number `hole`, which must be exactly one expression.
fn parse_hole(text: &str, hole: usize) -> Result<Expr> {
    let mut statements = parse_program(text)?;
    match statements.len() {
        0 => Err(hole_refusal(
            format!("interpolation hole {hole} is empty: `${{}}` holds no expression"),
            "write one expression inside the hole, or use a raw literal r\"...\" when the `${` is text",
        )),
        1 => match statements.pop() {
            Some(Stmt::Expr(expr)) => Ok(expr),
            _ => Err(hole_refusal(
                format!(
                    "interpolation hole {hole} holds a statement, not an expression: `{text}`"
                ),
                "a hole holds one expression; move the statement out of the string",
            )),
        },
        count => Err(hole_refusal(
            format!("interpolation hole {hole} holds {count} statements: `{text}`"),
            "a hole holds exactly one expression, so give the other statements their own line",
        )),
    }
}

/// The `E_PARSE` a hole inside a string literal is refused with.
fn hole_refusal(message: impl Into<String>, guidance: &str) -> anyhow::Error {
    anyhow::Error::new(ParseError::with_hint(message, guidance))
}

/// The index of the `}` that closes the hole whose text starts at `body`.
///
/// A quoted or raw region that never closed is refused as a hole with no
/// matching `}`: either way this hole has no end.
fn hole_end(source: &str, body: usize, hole: usize) -> Result<usize> {
    let mut depth = 1usize;
    let mut i = body;

    while i < source.len() {
        let ch = source[i..]
            .chars()
            .next()
            .expect("every index this loop reaches is a character boundary");
        let next = match ch {
            '\'' => end_of_single_quoted(source, i),
            '"' => end_of_double_quoted(source, i),
            'r' => match raw_prefix_len(source, i) {
                Some(hashes) => end_of_raw(source, i, hashes),
                None => Some(i + 1),
            },
            '#' => Some(if source[i + 1..].starts_with('[') {
                end_of_attribute(source, i)
            } else {
                end_of_line_comment(source, i)
            }),
            '/' if source[i + 1..].starts_with('/') => Some(end_of_line_comment(source, i)),
            '{' => {
                depth += 1;
                Some(i + 1)
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
                Some(i + 1)
            }
            other => Some(i + other.len_utf8()),
        };
        match next {
            Some(at) => i = at,
            None => return Err(unterminated_hole(hole)),
        }
    }

    Err(unterminated_hole(hole))
}

/// The refusal a hole with no matching `}` gets, reached either at the end of
/// the value or inside a quoted region that never closed.
fn unterminated_hole(hole: usize) -> anyhow::Error {
    hole_refusal(
        format!(
            "interpolation hole {hole} opens with `${{` and has no matching `}}`; no hole in this string was evaluated"
        ),
        "close the hole with `}`, or use a raw literal r\"...\" when the text is not a template",
    )
}

/// The index just past the `'` that closes the single-quoted string at `start`.
///
/// AetherShell gives `'...'` no escapes, so its body ends at the next `'`.
fn end_of_single_quoted(source: &str, start: usize) -> Option<usize> {
    let body = start + 1;
    source[body..].find('\'').map(|at| body + at + 1)
}

/// The index just past the `"` that closes the double-quoted string at `start`.
///
/// A backslash pairs with the character after it, so an escaped quote does not
/// close the string. Whether that escape is one the language accepts is the
/// expression parser's judgement, not this scan's.
fn end_of_double_quoted(source: &str, start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < source.len() {
        let ch = source[i..].chars().next()?;
        i += ch.len_utf8();
        match ch {
            '\\' => {
                let escaped = source[i..].chars().next()?;
                i += escaped.len_utf8();
            }
            '"' => return Some(i),
            _ => {}
        }
    }
    None
}

/// The number of `#` between the `r` at `at` and its opening quote, or `None`
/// where the lexer reads no raw literal: the identifier `r`, `raw`, a name
/// ending in `r`, a run over [`MAX_RAW_HASHES`], or no quote.
fn raw_prefix_len(source: &str, at: usize) -> Option<usize> {
    let preceding = source[..at].chars().next_back();
    if preceding.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let rest = &source[at + 1..];
    if rest.starts_with('"') {
        return Some(0);
    }
    let hashes = rest.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > MAX_RAW_HASHES {
        return None;
    }
    (rest.as_bytes().get(hashes) == Some(&b'"')).then_some(hashes)
}

/// The index just past the closing delimiter of the raw literal at `start`: a
/// `"` followed by exactly `hashes` `#`. Braces inside its text are content, so
/// a raw literal is how a `}` is put in a hole.
fn end_of_raw(source: &str, start: usize, hashes: usize) -> Option<usize> {
    let mut i = start + 1 + hashes + 1;
    while i < source.len() {
        let ch = source[i..].chars().next()?;
        if ch == '"'
            && source
                .as_bytes()
                .get(i + 1..i + 1 + hashes)
                .is_some_and(|run| run.iter().all(|b| *b == b'#'))
        {
            return Some(i + 1 + hashes);
        }
        i += ch.len_utf8();
    }
    None
}

/// The index just past the `]` that closes the `#[...]` attribute at `start`.
/// The lexer's own attribute scan counts brackets only.
fn end_of_attribute(source: &str, start: usize) -> usize {
    let mut i = start + 2;
    let mut depth = 1usize;
    while let Some(ch) = source[i..].chars().next() {
        i += ch.len_utf8();
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    i
}

/// The index of the newline ending the comment at `start`, or the end of the
/// value when the comment runs to it.
fn end_of_line_comment(source: &str, start: usize) -> usize {
    source[start..]
        .find('\n')
        .map_or(source.len(), |at| start + at)
}

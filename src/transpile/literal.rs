//! The one place the transpilers turn text they did not write into an
//! AetherShell string literal.
//!
//! Generated source is parsed by the real grammar, so a literal is only usable
//! if it uses escapes the grammar has and leaves no `${...}` the evaluator
//! would read as a hole. Three spellings cover every input:
//!
//! * the ordinary `"..."` form, byte-identical to what the transpilers emitted
//!   before this module existed, whenever it carries the text exactly;
//! * `r"..."` / `r#"..."#` with the fewest `#` delimiters, for `${` and for a
//!   control character the literal grammar cannot escape;
//! * the ordinary form with `${` written `${'$'}`, for text no raw literal can
//!   carry — it holds every `"#…` closing run through [`MAX_RAW_HASHES`].
//!
//! The last case is not hypothetical: a quote followed by seventeen `#`
//! characters blocks every raw delimiter, and leaving that text's `${1+2}` in
//! an ordinary literal would make the evaluator answer `3` where the text was
//! meant to be read.

/// The most `#` delimiters a raw literal may carry (`r"…"` through `r##…#"…"##…#`).
const MAX_RAW_HASHES: usize = 16;

/// The AetherShell spelling of `s`, ready to paste into generated source.
pub(crate) fn ae_source_string(s: &str) -> String {
    // Ordinary first, so generated source for ordinary text does not move. It
    // is inexact for `${`, which the evaluator reads as a hole, and for a
    // control character the grammar has no escape for.
    if !s.contains("${") && !has_unescapable_control(s) {
        return format!("\"{}\"", escaped_body(s, false));
    }
    raw_literal(s).unwrap_or_else(|| format!("\"{}\"", escaped_body(s, true)))
}

/// The escaped body for text the caller splices inside quotes it is building
/// itself. No quotes are added, and `${` is written `${'$'}` so the text cannot
/// start a hole.
pub(crate) fn ae_quoted_body(s: &str) -> String {
    escaped_body(s, true)
}

/// True when `s` carries a control character the literal grammar cannot escape
/// (`\n`, `\r` and `\t` are the three it can).
fn has_unescapable_control(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

/// The five escapes the literal grammar has, and every other character literal
/// — which is what the grammar means by it. With `protect_holes`, a `$` that
/// starts `${` is written `${'$'}` instead: the shell's own spelling of a
/// literal dollar, which interpolates to itself.
fn escaped_body(s: &str, protect_holes: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '$' if protect_holes && chars.peek() == Some(&'{') => out.push_str("${'$'}"),
            c => out.push(c),
        }
    }
    out
}

/// `r"..."`, or `r#"..."#` with the fewest `#` delimiters whose closing run is
/// not in the text; `None` when the text carries every run through
/// [`MAX_RAW_HASHES`], which leaves no raw spelling at all.
fn raw_literal(s: &str) -> Option<String> {
    for hashes in 0..=MAX_RAW_HASHES {
        let delimiter = "#".repeat(hashes);
        if !s.contains(&format!("\"{delimiter}")) {
            return Some(format!("r{delimiter}\"{s}\"{delimiter}"));
        }
    }
    None
}

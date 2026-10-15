//! Generated source must parse.
//!
//! The Bash, Zsh and PowerShell transpilers emit AetherShell string literals
//! from text they did not write. Three shapes of that text the literal grammar
//! refuses, each of which the corresponding escaper used to emit anyway:
//!
//! * a control character reached `json_string_literal`, which spelled it
//!   `\u0001` — a form the literal grammar has never had, so the program the
//!   user asked for was an `E_PARSE`;
//! * `${...}` in opaque text was emitted as an ordinary literal, so the
//!   evaluator read a hole and substituted a binding instead of the text;
//! * a backslash pair the grammar does not know (`\d`) was copied into the
//!   generated literal, which the lexer silently reduced to `d` and now
//!   refuses.
//!
//! Each case below transpiles, then runs `parse_program` on the output: the
//! contract is the generated program, not one line's spelling. Cases whose
//! value is pure (`echo` returns a String and performs no I/O) are evaluated
//! too, so the check is the value and not only the shape.

#![cfg(feature = "native")]

use aethershell::env::Env;
use aethershell::transpile::agentic::transpile_agentic_to_ae;
use aethershell::transpile::bash::transpile_bash_to_ae;
use aethershell::transpile::powershell::transpile_powershell_to_ae;
use aethershell::transpile::zsh::transpile_zsh_to_ae;
use aethershell::value::Value;

/// Transpile Bash, requiring the generated program to parse.
fn bash(src: &str) -> String {
    let ae = transpile_bash_to_ae(src).expect("transpile");
    parses(&ae)
}

/// Transpile Zsh, requiring the generated program to parse.
fn zsh(src: &str) -> String {
    let ae = transpile_zsh_to_ae(src).expect("transpile");
    parses(&ae)
}

/// Transpile PowerShell, requiring the generated program to parse.
fn pwsh(src: &str) -> String {
    let ae = transpile_powershell_to_ae(src).expect("transpile");
    parses(&ae)
}

/// Transpile Agentic, requiring the generated program to parse.
fn agentic(src: &str) -> String {
    let ae = transpile_agentic_to_ae(src).expect("transpile");
    parses(&ae)
}

fn parses(ae: &str) -> String {
    aethershell::parser::parse_program(ae)
        .unwrap_or_else(|e| panic!("generated source did not parse: {e}\n{ae}"));
    ae.to_string()
}

/// Evaluate a generated program whose value is one String.
fn value(ae: &str) -> String {
    let stmts = aethershell::parser::parse_program(ae).expect("parse");
    let mut env = Env::new();
    match aethershell::eval::eval_program(&stmts, &mut env).expect("eval") {
        Value::Str(s) => s,
        other => panic!("expected a string, got {other:?}"),
    }
}

// ── the ordinary spelling is kept where it is exact ─────────────────────

#[test]
fn bash_ascii_fallback_output_is_unchanged() {
    // The spelling existing callers and tests see must not move for text an
    // ordinary literal carries exactly.
    let ae = bash("echo hi > out.txt");
    assert!(
        ae.contains(r#"sh(["bash","-lc", "echo hi > out.txt"]);"#),
        "{ae}"
    );
}

#[test]
fn bash_environment_expansion_is_still_expansion() {
    let ae = bash(r#"echo "hello $USER""#);
    assert!(
        ae.contains(r#"echo("hello " + env("USER", ""))"#),
        "the environment read was rewritten: {ae}"
    );
}

#[test]
fn bash_windows_path_in_single_quotes_survives() {
    // The escaper doubles backslashes, so this text was always writable; the
    // case is here because it is the one an ordinary literal is judged on.
    let ae = bash(r"echo 'C:\Users\dev'");
    assert_eq!(value(&ae), r"C:\Users\dev");
}

// ── control characters: a raw literal, never `\u` ───────────────────────

#[test]
fn bash_control_character_in_a_fallback_line_is_not_a_u_escape() {
    let src = "printf 'a\u{1}b' > out";
    let ae = bash(src);
    assert!(
        !ae.contains("\\u"),
        "an unsupported escape reached the program: {ae}"
    );
    assert!(ae.contains(&format!("r\"{src}\"")), "{ae}");
}

#[test]
fn bash_control_character_in_a_single_quoted_argument_is_not_a_u_escape() {
    let ae = bash("echo 'a\u{1}b'");
    assert!(
        !ae.contains("\\u"),
        "an unsupported escape reached the program: {ae}"
    );
    assert!(ae.contains("echo(r\"a\u{1}b\")"), "{ae}");
}

#[test]
fn bash_control_character_in_an_external_argv_element_is_not_a_u_escape() {
    let ae = bash("myprog 'a\u{1}b'");
    assert!(
        !ae.contains("\\u"),
        "an unsupported escape reached the program: {ae}"
    );
    assert!(ae.contains("a\u{1}b"), "the character was lost: {ae}");
}

// ── `${...}` in opaque text is text ─────────────────────────────────────

#[test]
fn bash_single_quoted_braced_text_is_not_interpolated() {
    let ae = bash("echo 'a ${b} c'");
    assert!(ae.contains("echo(r\"a ${b} c\")"), "{ae}");
    assert_eq!(value(&ae), "a ${b} c");

    // Same text, ordinary spelling: the evaluator substitutes, which is the
    // value the generated program used to produce.
    assert_ne!(
        value("echo(\"a ${b} c\")"),
        "a ${b} c",
        "an ordinary literal no longer interpolates; this test proves nothing"
    );
}

#[test]
fn bash_braced_text_in_a_fallback_line_stays_text() {
    let ae = bash("echo a > f ${NAME}");
    assert!(
        ae.contains(r#"sh(["bash","-lc", r"echo a > f ${NAME}"]);"#),
        "{ae}"
    );
}

// ── Agentic literals keep the backslash the source wrote ────────────────

#[test]
fn agentic_single_quoted_backslash_is_data() {
    // `\d` is not one of the five escapes, so the backslash is doubled: the
    // value is what the source said, and the generated program parses. The old
    // emission copied `\d` bare, which dropped the backslash.
    let ae = agentic(r"e'a\db'");
    assert!(ae.contains(r#"echo("a\\db")"#), "{ae}");
    assert_eq!(value(&ae), r"a\db");
}

#[test]
fn agentic_double_quoted_backslash_is_data() {
    let ae = agentic(r#"e"a\db""#);
    assert!(ae.contains(r#"echo("a\\db")"#), "{ae}");
    assert_eq!(value(&ae), r"a\db");
}

#[test]
fn agentic_backtick_command_keeps_its_backslash() {
    let ae = agentic(r"x=`grep \d f`");
    assert!(ae.contains(r#"sh("grep \\d f")"#), "{ae}");
}

#[test]
fn agentic_escaped_backtick_stays_in_the_command() {
    // The old emission `sh("a\`b")` is now a refused escape, and the pair was
    // never a terminator, so it becomes a bare backtick.
    let ae = agentic(r"x=`a\`b`");
    assert!(ae.contains(r#"sh("a`b")"#), "{ae}");
}

#[test]
fn agentic_known_escapes_are_unchanged() {
    // `\n` is one of the five the literal grammar has, so the pair is copied
    // as written: the value is a newline, as before.
    let ae = agentic(r"e'a\nb'");
    assert!(ae.contains(r#"echo("a\nb")"#), "{ae}");
    assert_eq!(value(&ae), "a\nb");
}

#[test]
fn agentic_embedded_quotes_are_still_escaped() {
    let ae = agentic(r#"e'she said "hi"'"#);
    assert!(ae.contains(r#"echo("she said \"hi\"")"#), "{ae}");
    assert_eq!(value(&ae), "she said \"hi\"");
}

#[test]
fn agentic_single_quoted_braced_text_is_left_as_it_was() {
    // Agentic single quotes are converted to a double-quoted literal, whose
    // existing `${...}` behaviour is preserved rather than changed here.
    let ae = agentic("e'a ${b} c'");
    assert!(ae.contains("echo(\"a ${b} c\")"), "{ae}");
}

#[test]
fn agentic_unclosed_single_quote_stays_a_parse_error() {
    // The source literal never closes: this surface reads a terminal `\'` as an
    // escaped quote, so there is no closing quote to find. The transpiler must
    // not invent one — the generated program has to be an error too. Doubling
    // the stray backslash keeps it as data rather than letting it escape the
    // closing `)`.
    let ae = transpile_agentic_to_ae(r"e'a\").expect("transpile");
    assert!(ae.contains("echo(\"a\\\\)"), "{ae}");
    assert!(
        aethershell::parser::parse_program(&ae).is_err(),
        "an unclosed source literal produced a closed program: {ae}"
    );

    // The valid spelling of a trailing backslash closes the literal and holds
    // what the source wrote.
    let closed = agentic(r"e'a\\'");
    assert!(closed.contains(r#"echo("a\\")"#), "{closed}");
    assert_eq!(value(&closed), "a\\");
}

#[test]
fn agentic_escaped_single_quote_is_an_apostrophe() {
    // `\'` is this surface's escaped quote — it does not close the literal —
    // and the old emission `"it\'s"` is now a refused escape, so the pair has
    // to become a bare apostrophe.
    let ae = agentic(r"e'it\'s'");
    assert!(ae.contains(r#"echo("it's")"#), "{ae}");
    assert_eq!(value(&ae), "it's");
}

// ── Zsh and PowerShell share the same literal spelling ──────────────────

#[test]
fn zsh_braced_text_uses_a_raw_literal() {
    let ae = zsh("echo 'a ${b} c'");
    assert!(ae.contains("echo(r\"a ${b} c\")"), "{ae}");
    assert_eq!(value(&ae), "a ${b} c");
}

#[test]
fn zsh_control_character_is_not_a_u_escape() {
    let ae = zsh("echo 'a\u{1}b'");
    assert!(
        !ae.contains("\\u"),
        "an unsupported escape reached the program: {ae}"
    );
    assert_eq!(value(&ae), "a\u{1}b");
}

#[test]
fn powershell_braced_text_uses_a_raw_literal() {
    let ae = pwsh("Write-Host 'a ${b} c'");
    assert!(ae.contains("echo(r\"a ${b} c\")"), "{ae}");
    assert_eq!(value(&ae), "a ${b} c");
}

#[test]
fn powershell_control_character_is_not_a_u_escape() {
    let ae = pwsh("Write-Host 'a\u{1}b'");
    assert!(
        !ae.contains("\\u"),
        "an unsupported escape reached the program: {ae}"
    );
    assert_eq!(value(&ae), "a\u{1}b");
}

// ── text no raw literal can carry ───────────────────────────────────────

/// A quote followed by seventeen `#` characters contains the closing run
/// `"#…#` for every count from 0 through 16, so no raw literal can carry this
/// text. The `${1+2}` is what an ordinary literal would answer with `3`.
fn unrawable() -> String {
    format!("a\"{} ${{1+2}}", "#".repeat(17))
}

/// Every shell transpiler must hand this text back unchanged, both because the
/// literal has to parse and because the evaluator must read the hole as text.
fn assert_round_trips(ae: &str, what: &str) {
    let text = unrawable();
    assert!(
        !ae.contains("${1+2}"),
        "{what}: a hole reached the program: {ae}"
    );
    assert!(
        ae.contains("${'$'}"),
        "{what}: the dollar was not written as a literal: {ae}"
    );
    assert_eq!(value(ae), text, "{what}: the text was rewritten");
    assert_ne!(
        value(ae),
        text.replace("${1+2}", "3"),
        "{what}: the literal was interpolated"
    );
}

#[test]
fn bash_unrawable_text_round_trips() {
    let ae = bash(&format!("echo '{}'", unrawable()));
    assert_round_trips(&ae, "bash");
}

#[test]
fn zsh_unrawable_text_round_trips() {
    let ae = zsh(&format!("echo '{}'", unrawable()));
    assert_round_trips(&ae, "zsh");
}

#[test]
fn powershell_unrawable_text_round_trips() {
    let ae = pwsh(&format!("Write-Host '{}'", unrawable()));
    assert_round_trips(&ae, "powershell");
}

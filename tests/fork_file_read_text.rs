//! `file_read_text` end to end: the option contract, the raw snapshot, the
//! byte-order-mark policies, the decoders and the refusals.
//!
//! Every path this file touches lives in a private directory it creates itself
//! with `create_dir` — exclusive, nonce- and counter-named, never a shared or
//! PID-only location — and removes on every path out of the test, including a
//! panicking one. Nothing here writes outside that directory, changes the
//! process environment, or changes the working directory, so the suite is safe
//! to run beside any other.
//!
//! The byte vectors are literals rather than encoded strings, so a test asserts
//! the exact bytes on disk and the exact bytes the builtin reports. Where a hash
//! is checked the test hashes the file itself with `sha2`, so the digest is
//! verified against an independent computation rather than against the module's
//! own.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aethershell::builtins;
use aethershell::env::Env;
use aethershell::file_read_text;
use aethershell::safety;
use aethershell::value::Value;

// ── the private directory ───────────────────────────────────────────────

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

/// A directory this test owns, removed when the test ends.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos() as u64)
            .unwrap_or(0);
        for _ in 0..64u32 {
            let counter = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                "ae_fork_file_read_text_{nonce:016x}_{:08x}_{counter:08x}",
                std::process::id()
            );
            let path = std::env::temp_dir().join(name);
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create {}: {error}", path.display()),
            }
        }
        panic!("could not allocate a private directory in 64 attempts");
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Write one fixture and answer its path.
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes)
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", path.display()));
        path
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ── driving the builtin ─────────────────────────────────────────────────

fn require_human_mode() {
    assert_eq!(
        safety::current_mode(),
        safety::Mode::Human,
        "run this suite in the default human mode; the workspace jail is not under test here"
    );
}

fn as_value(path: &Path) -> Value {
    Value::Str(path.to_string_lossy().into_owned())
}

fn options(pairs: &[(&str, Value)]) -> Value {
    Value::Record(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect(),
    )
}

fn run_raw(args: Vec<Value>, input: Option<Value>) -> anyhow::Result<Value> {
    require_human_mode();
    file_read_text::run(args, input)
}

fn run(path: &Path, options: Option<Value>) -> anyhow::Result<Value> {
    let args = match options {
        Some(options) => vec![as_value(path), options],
        None => vec![as_value(path)],
    };
    run_raw(args, None)
}

fn read_record(path: &Path, options: Option<Value>) -> BTreeMap<String, Value> {
    match run(path, options) {
        Ok(Value::Record(record)) => record,
        Ok(other) => panic!("file_read_text answered {other:?} instead of a result record"),
        Err(error) => panic!("file_read_text failed for {}: {error}", path.display()),
    }
}

/// The message of a refusal, which carries the code the caller branches on.
fn refusal(path: &Path, options: Option<Value>) -> String {
    match run(path, options) {
        Err(error) => error.to_string(),
        Ok(value) => panic!("file_read_text accepted a call it must refuse: {value:?}"),
    }
}

// ── reading the record ──────────────────────────────────────────────────

fn field<'a>(record: &'a BTreeMap<String, Value>, key: &str) -> &'a Value {
    record
        .get(key)
        .unwrap_or_else(|| panic!("`{key}` is missing from {record:?}"))
}

fn record_of<'a>(record: &'a BTreeMap<String, Value>, key: &str) -> &'a BTreeMap<String, Value> {
    match field(record, key) {
        Value::Record(inner) => inner,
        other => panic!("`{key}` is {other:?}, not a Record"),
    }
}

fn int(record: &BTreeMap<String, Value>, key: &str) -> i64 {
    match field(record, key) {
        Value::Int(number) => *number,
        other => panic!("`{key}` is {other:?}, not an Int"),
    }
}

fn flag(record: &BTreeMap<String, Value>, key: &str) -> bool {
    match field(record, key) {
        Value::Bool(value) => *value,
        other => panic!("`{key}` is {other:?}, not a Bool"),
    }
}

fn text(record: &BTreeMap<String, Value>, key: &str) -> String {
    match field(record, key) {
        Value::Str(value) => value.clone(),
        other => panic!("`{key}` is {other:?}, not a String"),
    }
}

/// A field that may be an explicit JSON null.
fn optional_text(record: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    match field(record, key) {
        Value::Str(value) => Some(value.clone()),
        Value::Null => None,
        other => panic!("`{key}` is {other:?}, neither a String nor null"),
    }
}

fn optional_flag(record: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    match field(record, key) {
        Value::Bool(value) => Some(*value),
        Value::Null => None,
        other => panic!("`{key}` is {other:?}, neither a Bool nor null"),
    }
}

/// The bytes the record actually carries, decoded from its base64.
fn snapshot(record: &BTreeMap<String, Value>) -> Vec<u8> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    STANDARD
        .decode(text(record, "base64"))
        .unwrap_or_else(|error| panic!("base64 is not base64: {error}"))
}

// ── the fixtures ────────────────────────────────────────────────────────

/// Chinese, an emoji, every newline spelling, a tab, a NUL, a backslash and a
/// shell expansion that a re-quoting layer would have eaten.
const RAW: &[u8] = "中文 🙂\r\nline2\nline3\rcafé — ✓\tend\0back\\slash${VAR}".as_bytes();
const RAW_TEXT: &str = "中文 🙂\r\nline2\nline3\rcafé — ✓\tend\0back\\slash${VAR}";
/// The UTF-8 mark.
const MARK_UTF8: &[u8] = &[0xEF, 0xBB, 0xBF];
/// A UTF-16LE mark followed by `A`.
const MARK_UTF16LE: &[u8] = &[0xFF, 0xFE, 0x41, 0x00];
/// A UTF-16BE mark followed by `A`.
const MARK_UTF16BE: &[u8] = &[0xFE, 0xFF, 0x00, 0x41];
/// `中文测试` in GBK: not valid UTF-8, so a guessing decoder is caught here.
const GBK: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4];
/// `A`, then a truncated two-byte sequence: malformed in UTF-8.
const MALFORMED: &[u8] = &[0x41, 0xC3, 0x28, 0x42];
/// U+FFFD really encoded in the file: valid content, not a substitution.
const REAL_REPLACEMENT: &[u8] = &[0xEF, 0xBF, 0xBD];

// ── the contract ────────────────────────────────────────────────────────

#[test]
fn utf8_text_and_bytes_survive_the_round_trip_without_rewriting_newlines() {
    let dir = PrivateDir::new();
    let path = dir.write("probe.txt", RAW);

    let record = read_record(&path, None);
    assert!(flag(&record, "success"), "{record:?}");
    assert_eq!(
        text(&record, "path"),
        path.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(int(&record, "bytes"), RAW.len() as i64);
    assert_eq!(snapshot(&record), RAW, "the raw bytes must be unchanged");
    assert_eq!(text(&record, "requested_encoding"), "utf-8");
    assert_eq!(text(&record, "encoding"), "utf-8");
    assert_eq!(optional_flag(&record, "decoded"), Some(true));
    assert_eq!(optional_text(&record, "text").as_deref(), Some(RAW_TEXT));
    assert!(flag(&record, "complete"));
    assert_eq!(optional_text(&record, "error"), None);

    let bom = record_of(&record, "bom");
    assert_eq!(optional_text(bom, "detected"), None);
    assert_eq!(int(bom, "bytes"), 0);
    assert_eq!(text(bom, "policy"), "preserve");
}

#[test]
fn the_digest_is_checked_against_an_independent_hash_of_the_file() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use sha2::{Digest, Sha256};

    let dir = PrivateDir::new();
    let path = dir.write("digest.bin", RAW);
    let record = read_record(&path, None);

    // Read the file again, outside the builtin, and hash those bytes here.
    let on_disk = std::fs::read(&path).expect("read the fixture back");
    let mut hasher = Sha256::new();
    hasher.update(&on_disk);
    let expected = hex::encode(hasher.finalize());

    assert_eq!(
        text(&record, "sha256"),
        expected,
        "the digest is not the file's"
    );
    assert_eq!(
        text(&record, "sha256"),
        text(&record, "sha256").to_lowercase(),
        "the digest must be lower-case hex"
    );
    assert_eq!(
        text(&record, "base64"),
        STANDARD.encode(&on_disk),
        "base64 is over the same bytes the digest covers"
    );
    assert_eq!(snapshot(&record), on_disk);
    assert_eq!(int(&record, "bytes"), on_disk.len() as i64);
}

#[test]
fn a_utf8_mark_is_preserved_stripped_required_or_forbidden() {
    let dir = PrivateDir::new();
    let path = dir.write("marked.txt", &[MARK_UTF8, b"hi"].concat());

    let preserved = read_record(&path, None);
    assert_eq!(
        optional_text(&preserved, "text").as_deref(),
        Some("\u{feff}hi")
    );
    assert_eq!(optional_flag(&preserved, "decoded"), Some(true));
    let bom = record_of(&preserved, "bom");
    assert_eq!(optional_text(bom, "detected").as_deref(), Some("utf-8"));
    assert_eq!(int(bom, "bytes"), 3, "the mark is three bytes");
    assert_eq!(text(bom, "policy"), "preserve");

    let strip = read_record(&path, Some(options(&[("bom", Value::Str("strip".into()))])));
    assert_eq!(optional_text(&strip, "text").as_deref(), Some("hi"));
    assert_eq!(
        int(&strip, "bytes"),
        5,
        "the snapshot is the whole file, mark included"
    );
    assert_eq!(snapshot(&strip).len(), 5);

    let require = read_record(
        &path,
        Some(options(&[("bom", Value::Str("require".into()))])),
    );
    assert!(flag(&require, "success"), "{require:?}");
    assert_eq!(
        optional_text(&require, "text").as_deref(),
        Some("\u{feff}hi"),
        "require is presence plus preserve, never a strip"
    );

    let forbid = read_record(
        &path,
        Some(options(&[("bom", Value::Str("forbid".into()))])),
    );
    assert!(!flag(&forbid, "success"), "{forbid:?}");
    assert_eq!(optional_flag(&forbid, "decoded"), Some(false));
    assert_eq!(optional_text(&forbid, "text"), None);
    assert_eq!(optional_text(&forbid, "encoding").as_deref(), Some("utf-8"));
    assert!(optional_text(&forbid, "error")
        .expect("says why")
        .contains("forbid"));
    assert_eq!(snapshot(&forbid).len(), 5, "the raw bytes are untouched");
}

#[test]
fn a_utf16_mark_decodes_in_both_orders_and_only_under_the_right_policy() {
    let dir = PrivateDir::new();
    for (name, bytes, codec) in [
        ("le.bin", MARK_UTF16LE, "utf-16le"),
        ("be.bin", MARK_UTF16BE, "utf-16be"),
    ] {
        let path = dir.write(name, bytes);

        let preserved = read_record(
            &path,
            Some(options(&[("encoding", Value::Str(codec.into()))])),
        );
        assert!(flag(&preserved, "success"), "{codec}: {preserved:?}");
        assert_eq!(
            optional_text(&preserved, "text").as_deref(),
            Some("\u{feff}A")
        );
        assert_eq!(
            optional_text(&preserved, "encoding").as_deref(),
            Some(codec)
        );
        assert_eq!(
            optional_text(&preserved, "requested_encoding").as_deref(),
            Some(codec)
        );

        let strip = read_record(
            &path,
            Some(options(&[
                ("encoding", Value::Str(codec.into())),
                ("bom", Value::Str("strip".into())),
            ])),
        );
        assert_eq!(optional_text(&strip, "text").as_deref(), Some("A"));
        assert_eq!(snapshot(&strip), bytes);

        // The same bytes decoded as the other order are garbage, and the two
        // mismatched bytes are what says so.
        let other = if codec == "utf-16le" {
            "utf-16be"
        } else {
            "utf-16le"
        };
        let clash = read_record(
            &path,
            Some(options(&[("encoding", Value::Str(other.into()))])),
        );
        assert!(!flag(&clash, "success"), "{other}: {clash:?}");
        assert_eq!(optional_flag(&clash, "decoded"), Some(false));
    }
}

#[test]
fn gbk_bytes_need_the_gbk_codec_and_utf8_never_guesses_them() {
    let dir = PrivateDir::new();
    let path = dir.write("gbk.bin", GBK);

    let gbk = read_record(
        &path,
        Some(options(&[("encoding", Value::Str("gbk".into()))])),
    );
    assert!(flag(&gbk, "success"), "{gbk:?}");
    assert_eq!(optional_text(&gbk, "text").as_deref(), Some("中文测试"));
    assert_eq!(optional_flag(&gbk, "decoded"), Some(true));
    assert_eq!(snapshot(&gbk), GBK);

    // The default UTF-8 is a decoding failure, not a substitution and not a
    // failure to read: the bytes survive it.
    let utf8 = read_record(&path, None);
    assert!(!flag(&utf8, "success"), "{utf8:?}");
    assert_eq!(optional_flag(&utf8, "decoded"), Some(false));
    assert_eq!(optional_text(&utf8, "text"), None);
    assert_eq!(optional_text(&utf8, "encoding").as_deref(), Some("utf-8"));
    assert_eq!(snapshot(&utf8), GBK);
    assert!(
        flag(&utf8, "complete"),
        "a decode failure is not a read failure"
    );
    assert!(!optional_text(&utf8, "error").expect("says why").is_empty());
}

#[test]
fn bytes_mode_reports_the_snapshot_and_no_decoding_at_all() {
    let dir = PrivateDir::new();
    let path = dir.write("raw.bin", &[MARK_UTF8, GBK].concat());

    let record = read_record(
        &path,
        Some(options(&[("encoding", Value::Str("bytes".into()))])),
    );
    assert!(flag(&record, "success"), "{record:?}");
    assert_eq!(int(&record, "bytes"), (MARK_UTF8.len() + GBK.len()) as i64);
    assert_eq!(snapshot(&record), [MARK_UTF8, GBK].concat());
    assert_eq!(text(&record, "encoding"), "bytes");
    assert_eq!(text(&record, "requested_encoding"), "bytes");
    assert_eq!(optional_flag(&record, "decoded"), None);
    assert_eq!(optional_text(&record, "text"), None);
    assert_eq!(optional_text(&record, "error"), None);
    assert!(flag(&record, "complete"));

    // The detected mark is still reported exactly, and its byte count is the
    // mark's own length rather than the file's.
    let bom = record_of(&record, "bom");
    assert_eq!(optional_text(bom, "detected").as_deref(), Some("utf-8"));
    assert_eq!(int(bom, "bytes"), 3);
    assert_eq!(text(bom, "policy"), "preserve");

    // A policy that decides what a decode does with a mark is meaningless here.
    for policy in ["strip", "require", "forbid"] {
        let message = refusal(
            &path,
            Some(options(&[
                ("encoding", Value::Str("bytes".into())),
                ("bom", Value::Str(policy.into())),
            ])),
        );
        assert!(message.contains("E_BAD_ARG"), "{policy}: {message}");
    }
    // The omitted default is fine, and so is spelling it out.
    assert!(flag(
        &read_record(
            &path,
            Some(options(&[
                ("encoding", Value::Str("bytes".into())),
                ("bom", Value::Str("preserve".into())),
            ])),
        ),
        "success"
    ));
}

#[test]
fn a_mark_that_contradicts_the_named_codec_is_a_recorded_refusal() {
    let dir = PrivateDir::new();
    let path = dir.write("be.bin", MARK_UTF16BE);

    for named in ["utf-8", "gbk", "utf-16le"] {
        let record = read_record(
            &path,
            Some(options(&[("encoding", Value::Str(named.into()))])),
        );
        assert!(!flag(&record, "success"), "{named}: {record:?}");
        assert_eq!(optional_flag(&record, "decoded"), Some(false), "{named}");
        assert_eq!(optional_text(&record, "text"), None, "{named}");
        assert_eq!(
            optional_text(&record, "encoding").as_deref(),
            Some(named),
            "the requested name stays reported"
        );
        assert!(
            optional_text(&record, "error")
                .expect("says why")
                .contains("contradicts"),
            "{named}: the refusal must name the conflict"
        );
        assert_eq!(snapshot(&record), MARK_UTF16BE, "{named}: raw kept");
    }
}

#[test]
fn bom_encoding_needs_a_recognized_mark_and_names_the_one_it_read() {
    let dir = PrivateDir::new();
    let plain = dir.write("plain.txt", b"plain utf-8\n");
    let marked = dir.write("le.bin", MARK_UTF16LE);

    let without = read_record(
        &plain,
        Some(options(&[("encoding", Value::Str("bom".into()))])),
    );
    assert!(!flag(&without, "success"), "{without:?}");
    assert_eq!(optional_flag(&without, "decoded"), Some(false));
    assert_eq!(
        optional_text(&without, "encoding"),
        None,
        "nothing was detected, so no codec can be named"
    );
    assert_eq!(optional_text(&without, "text"), None);
    assert_eq!(snapshot(&without), b"plain utf-8\n".as_slice());
    assert_eq!(text(&without, "requested_encoding"), "bom");

    let detected = read_record(
        &marked,
        Some(options(&[("encoding", Value::Str("bom".into()))])),
    );
    assert!(flag(&detected, "success"), "{detected:?}");
    assert_eq!(
        optional_text(&detected, "encoding").as_deref(),
        Some("utf-16le")
    );
    assert_eq!(
        optional_text(&detected, "text").as_deref(),
        Some("\u{feff}A")
    );
    let bom = record_of(&detected, "bom");
    assert_eq!(optional_text(bom, "detected").as_deref(), Some("utf-16le"));
    assert_eq!(int(bom, "bytes"), 2);

    let stripped = read_record(
        &marked,
        Some(options(&[
            ("encoding", Value::Str("bom".into())),
            ("bom", Value::Str("strip".into())),
        ])),
    );
    assert_eq!(optional_text(&stripped, "text").as_deref(), Some("A"));
    assert_eq!(
        optional_text(record_of(&stripped, "bom"), "detected").as_deref(),
        Some("utf-16le"),
        "the mark is still reported after it is stripped"
    );
}

#[test]
fn malformed_bytes_are_a_recorded_refusal_and_a_real_replacement_is_content() {
    let dir = PrivateDir::new();
    let broken = dir.write("broken.bin", MALFORMED);
    let replacement = dir.write("replacement.txt", REAL_REPLACEMENT);

    let record = read_record(&broken, None);
    assert!(!flag(&record, "success"), "{record:?}");
    assert_eq!(optional_flag(&record, "decoded"), Some(false));
    assert_eq!(optional_text(&record, "text"), None);
    assert_eq!(
        snapshot(&record),
        MALFORMED,
        "no replacement was substituted"
    );
    assert_eq!(int(&record, "bytes"), MALFORMED.len() as i64);

    // U+FFFD written on purpose is valid UTF-8 content, not a decode failure.
    let good = read_record(&replacement, None);
    assert!(flag(&good, "success"), "{good:?}");
    assert_eq!(optional_flag(&good, "decoded"), Some(true));
    assert_eq!(optional_text(&good, "text").as_deref(), Some("\u{fffd}"));
    assert_eq!(optional_text(&good, "error"), None);
}

#[test]
fn a_file_past_max_bytes_is_refused_rather_than_reported_in_part() {
    let dir = PrivateDir::new();
    let path = dir.write("five.bin", b"abcde");

    let message = refusal(&path, Some(options(&[("max_bytes", Value::Int(4))])));
    assert!(
        message.contains("E_BUDGET_EXCEEDED"),
        "an oversized read is a budget refusal, not a malformed call: {message}"
    );
    assert!(!message.contains("E_BAD_ARG"), "{message}");
    assert!(
        message.contains("max_bytes"),
        "must name the bound: {message}"
    );

    // The bound is inclusive: exactly `max_bytes` is read whole.
    let at_the_bound = read_record(&path, Some(options(&[("max_bytes", Value::Int(5))])));
    assert!(flag(&at_the_bound, "success"), "{at_the_bound:?}");
    assert_eq!(int(&at_the_bound, "bytes"), 5);
    assert_eq!(snapshot(&at_the_bound), b"abcde".as_slice());

    // A `max_bytes` outside its range is the malformed call, and stays one.
    let over = refusal(&path, Some(options(&[("max_bytes", Value::Int(0))])));
    assert!(over.contains("E_BAD_ARG"), "{over}");
    assert!(!over.contains("E_BUDGET_EXCEEDED"), "{over}");
    let huge = refusal(
        &path,
        Some(options(&[("max_bytes", Value::Int(16_777_217))])),
    );
    assert!(huge.contains("E_BAD_ARG"), "{huge}");
    assert!(!huge.contains("E_BUDGET_EXCEEDED"), "{huge}");
}

#[test]
fn options_are_validated_before_a_missing_file_is_looked_up() {
    let dir = PrivateDir::new();
    let missing = dir.path().join("nothing-here.txt");

    for (call_options, why) in [
        (options(&[("unknown", Value::Int(1))]), "an unknown key"),
        (
            options(&[("encoding", Value::Str("latin-1".into()))]),
            "an unknown encoding",
        ),
        (
            options(&[("bom", Value::Str("drop".into()))]),
            "an unknown bom label",
        ),
        (
            options(&[("max_bytes", Value::Str("big".into()))]),
            "max_bytes is not an Int",
        ),
    ] {
        let message = refusal(&missing, Some(call_options));
        assert!(message.contains("E_BAD_ARG"), "{why}: {message}");
        assert!(
            !message.contains("E_NOT_FOUND"),
            "{why}: a malformed call must not be reported as a missing file: {message}"
        );
    }

    // The same path with a well-formed call is the absent file, coded.
    let message = refusal(&missing, None);
    assert!(message.contains("E_NOT_FOUND"), "not coded: {message}");
    assert!(
        !message.contains("E_UNKNOWN"),
        "absence must not arrive unclassified: {message}"
    );
}

#[test]
fn a_missing_file_and_a_missing_parent_are_not_found_and_nothing_else() {
    let dir = PrivateDir::new();

    for missing in [
        dir.path().join("absent.txt"),
        dir.path().join("no-dir").join("absent.txt"),
    ] {
        let message = refusal(&missing, None);
        assert!(
            message.contains("E_NOT_FOUND"),
            "{}: {message}",
            missing.display()
        );
        assert!(
            !message.contains("E_UNKNOWN") && !message.contains("canonicalize"),
            "a missing parent must not surface as an unclassified canonicalize failure: {message}"
        );
    }
}

#[test]
fn a_directory_is_refused_rather_than_read() {
    let dir = PrivateDir::new();
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).expect("create the nested directory");

    let message = refusal(&nested, None);
    assert!(message.contains("E_BAD_ARG"), "not coded: {message}");
    assert!(
        message.contains("regular file"),
        "the refusal must say what a path must name: {message}"
    );
}

#[test]
fn wrong_argument_shapes_are_refused_before_the_file_is_touched() {
    let dir = PrivateDir::new();
    let path = dir.write("probe.txt", b"ok");

    for (args, why) in [
        (vec![Value::Int(1)], "the path is not a String"),
        (vec![], "there is no path at all"),
        (
            vec![as_value(&path), options(&[]), options(&[])],
            "a third argument",
        ),
        (
            vec![as_value(&path), Value::Array(vec![])],
            "options is not a Record",
        ),
    ] {
        let message = match run_raw(args, None) {
            Err(error) => error.to_string(),
            Ok(value) => panic!("{why}: accepted {value:?}"),
        };
        assert!(message.contains("E_BAD_ARG"), "{why}: {message}");
    }

    // A piped subject is refused: the path is the call.
    let message = match run_raw(vec![as_value(&path)], Some(Value::Str("piped".into()))) {
        Err(error) => error.to_string(),
        Ok(value) => panic!("a piped subject was accepted: {value:?}"),
    };
    assert!(message.contains("E_BAD_ARG"), "{message}");
    assert!(message.contains("piped"), "{message}");
}

// ── the dispatcher ──────────────────────────────────────────────────────

#[test]
fn the_builtin_is_reachable_through_the_dispatcher_and_refusals_stay_coded() {
    let dir = PrivateDir::new();
    let path = dir.write("dispatched.txt", &[MARK_UTF8, b"hi"].concat());
    require_human_mode();

    let result = builtins::call(
        "file_read_text",
        vec![
            as_value(&path),
            options(&[
                ("encoding", Value::Str("bom".into())),
                ("bom", Value::Str("strip".into())),
            ]),
        ],
        &mut Env::new(),
    );
    match result {
        Ok(Value::Record(record)) => {
            assert!(flag(&record, "success"), "{record:?}");
            assert_eq!(optional_text(&record, "text").as_deref(), Some("hi"));
            assert_eq!(
                optional_text(&record, "encoding").as_deref(),
                Some("utf-8"),
                "the dispatcher call reaches the same decoder"
            );
        }
        Ok(other) => panic!("file_read_text answered {other:?} instead of a record"),
        Err(error) => panic!("the dispatcher did not reach file_read_text: {error}"),
    }

    // A refusal raised through the same door is still a coded one.
    let denied = builtins::call(
        "file_read_text",
        vec![as_value(&dir.path().join("absent.txt"))],
        &mut Env::new(),
    );
    match denied {
        Err(error) => {
            let message = error.to_string();
            assert!(message.contains("E_NOT_FOUND"), "not coded: {message}");
        }
        Ok(value) => panic!("a missing file was accepted: {value:?}"),
    }
}

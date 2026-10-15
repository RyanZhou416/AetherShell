//! `proc_run` end to end: the strict argv contract, the byte capture, the
//! decoder, the exit facts, the bounds and the cleanup.
//!
//! Every process this file starts is this test binary itself, run with
//! `--exact <fixture> --ignored --nocapture`. That keeps the tests free of any
//! host program (`cmd`, `sh`, PowerShell, `node`) and of a second compiled
//! artifact, and it gives a child whose exit code, streams and descendants are
//! all under the test's control. The fixtures are `#[ignore]`d, so a plain
//! `cargo test` runs none of them.
//!
//! libtest writes its own prologue to the child's **stdout** and nothing to its
//! stderr, so byte-exact assertions are made against the stderr stream and the
//! stdout assertions accept the prologue as a prefix. Each fixture writes its
//! payload and then exits directly, which is also why the fixtures flush
//! explicitly: `process::exit` runs no destructors.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aethershell::env::Env;
use aethershell::process_run;
use aethershell::safety;
use aethershell::value::Value;

// ── what the fixtures send ──────────────────────────────────────────────

const UTF8_PAYLOAD: &str = "héllo — ✓\n";
const UTF8_STDOUT: &str = "out-α\n";
/// `中文测试` followed by a newline, in GBK: not valid UTF-8, so a decoder that
/// guessed or substituted would be caught here.
const GBK_BYTES: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4, b'\n'];
const GBK_TEXT: &str = "中文测试\n";
/// A little-endian BOM, `A`, `é`. The mark is part of the stream.
const UTF16LE_BOM_BYTES: &[u8] = &[0xFF, 0xFE, 0x41, 0x00, 0xE9, 0x00];
/// The same text big-endian.
const UTF16BE_BOM_BYTES: &[u8] = &[0xFE, 0xFF, 0x00, 0x41, 0x00, 0xE9];
const UTF16_BOM_TEXT: &str = "\u{feff}A\u{e9}";
/// `A`, then a truncated two-byte sequence: malformed in UTF-8.
const INVALID_BYTES: &[u8] = &[0x41, 0xC3, 0x28, 0x42];
/// How much `fixture_noisy` writes to stderr: far past any limit used here.
const NOISY_BYTES: usize = 262_144;
/// Shell metacharacters. A shell — or anything that re-quotes the argv — would
/// expand `$HOME`, split on `|;><&` or glob `*`; the fixture must receive this
/// string unchanged.
const SHELL_PAYLOAD: &str = "a|b;c*d$HOME x>y<z&w";

// ── fixtures: only ever started by a test that passes `--ignored` ───────

#[test]
#[ignore]
fn fixture_utf8_both_streams() {
    write_stderr(UTF8_PAYLOAD.as_bytes());
    write_stdout(UTF8_STDOUT.as_bytes());
    std::process::exit(0);
}

#[test]
#[ignore]
fn fixture_gbk() {
    write_stderr(GBK_BYTES);
    std::process::exit(0);
}

#[test]
#[ignore]
fn fixture_utf16le_bom() {
    write_stderr(UTF16LE_BOM_BYTES);
    std::process::exit(0);
}

#[test]
#[ignore]
fn fixture_utf16be_bom() {
    write_stderr(UTF16BE_BOM_BYTES);
    std::process::exit(0);
}

#[test]
#[ignore]
fn fixture_invalid_utf8() {
    write_stderr(INVALID_BYTES);
    std::process::exit(0);
}

#[test]
#[ignore]
fn fixture_exit_code_three() {
    write_stderr(b"boom\n");
    std::process::exit(3);
}

#[test]
#[ignore]
fn fixture_noisy() {
    use std::io::Write;

    let mut stderr = std::io::stderr().lock();
    let chunk = [b'z'; 8192];
    let mut written = 0;
    while written < NOISY_BYTES {
        stderr.write_all(&chunk).expect("write to stderr");
        written += chunk.len();
    }
    stderr.flush().expect("flush stderr");
    std::process::exit(0);
}

#[test]
#[ignore]
fn fixture_sleeps() {
    write_stderr(b"awake\n");
    std::thread::sleep(Duration::from_secs(30));
    std::process::exit(0);
}

/// A leader that leaves a descendant holding both inherited pipes and exits.
#[test]
#[ignore]
fn fixture_tree() {
    let exe = std::env::current_exe().expect("the test executable path");
    Command::new(exe)
        .args(["--exact", "fixture_hold", "--ignored", "--nocapture"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start the descendant");
    std::process::exit(0);
}

/// The descendant: it writes once and then keeps the pipes open.
#[test]
#[ignore]
fn fixture_hold() {
    write_stderr(b"held\n");
    std::thread::sleep(Duration::from_secs(20));
    std::process::exit(0);
}

/// Echoes the value smuggled in on `--skip` to both streams.
#[test]
#[ignore]
fn fixture_echo_argument() {
    let value = smuggled_argument();
    write_stdout(value.as_bytes());
    write_stderr(value.as_bytes());
    std::process::exit(0);
}

/// Writes a file at the path smuggled in on `--skip`, so a test can prove that
/// a rejected call started nothing.
#[test]
#[ignore]
fn fixture_marker() {
    if let Some(path) = smuggled_argument_opt() {
        std::fs::write(path, b"started").expect("write the marker");
    }
    std::process::exit(0);
}

fn smuggled_argument_opt() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == "--skip")
        .and_then(|position| args.get(position + 1))
        .cloned()
}

fn smuggled_argument() -> String {
    smuggled_argument_opt().unwrap_or_default()
}

/// Runs one nested `proc_run` in whatever mode this process inherited, and
/// reports the outcome on stderr. The mode is set on this process by its parent
/// rather than by the test process's own environment, which is process-global.
#[test]
#[ignore]
fn fixture_agent_guard() {
    let marker = smuggled_argument();
    let argv = Value::Array(
        [
            executable(),
            "--exact".to_string(),
            "fixture_marker".to_string(),
            "--ignored".to_string(),
            "--nocapture".to_string(),
            "--skip".to_string(),
            marker.clone(),
        ]
        .into_iter()
        .map(Value::Str)
        .collect(),
    );

    let report = match process_run::run(vec![argv], None) {
        Ok(_) => "answer=ok".to_string(),
        Err(error) => format!("answer=err code={}", error_code(&error.to_string())),
    };
    let started = std::path::Path::new(&marker).exists();
    write_stderr(format!("{report} marker={started}\n").as_bytes());
    std::process::exit(0);
}

/// The first `E_...` code in an error's rendered form.
fn error_code(message: &str) -> String {
    match message.find("E_") {
        Some(at) => message[at..]
            .chars()
            .take_while(|character| character.is_ascii_uppercase() || *character == '_')
            .collect(),
        None => "none".to_string(),
    }
}

fn write_stderr(bytes: &[u8]) {
    use std::io::Write;

    let mut stderr = std::io::stderr().lock();
    stderr.write_all(bytes).expect("write to stderr");
    stderr.flush().expect("flush stderr");
}

fn write_stdout(bytes: &[u8]) {
    use std::io::Write;

    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes).expect("write to stdout");
    stdout.flush().expect("flush stdout");
}

// ── driving the builtin ─────────────────────────────────────────────────

/// The default native mode is the human REPL, where `Exec` is allowed. These
/// tests are about the module, not the gate (`tests/exec_gate_coverage.rs`
/// covers that), so a host that selected an agent policy is stated as a
/// precondition: this suite never changes the environment it was started with.
fn require_human_mode() {
    assert_eq!(
        safety::current_mode(),
        safety::Mode::Human,
        "run this suite in the default human mode; do not inherit agent controls"
    );
}

fn executable() -> String {
    std::env::current_exe()
        .expect("the test executable path")
        .to_string_lossy()
        .into_owned()
}

/// The argv that makes this test binary run exactly one ignored fixture.
fn fixture(name: &str) -> Value {
    Value::Array(
        [
            executable(),
            "--exact".to_string(),
            name.to_string(),
            "--ignored".to_string(),
            "--nocapture".to_string(),
        ]
        .into_iter()
        .map(Value::Str)
        .collect(),
    )
}

/// The fixture argv with one value appended for the fixture to read.
///
/// libtest refuses unknown arguments, so the value rides in on `--skip`, which
/// takes a filter that matches no test name.
fn fixture_with_value(name: &str, value: &str) -> Value {
    let Value::Array(mut items) = fixture(name) else {
        unreachable!("fixture() builds an array")
    };
    items.push(Value::Str("--skip".to_string()));
    items.push(Value::Str(value.to_string()));
    Value::Array(items)
}

fn options(pairs: &[(&str, Value)]) -> Value {
    Value::Record(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect(),
    )
}

fn run(argv: Value, options: Option<Value>) -> anyhow::Result<Value> {
    require_human_mode();
    let args = match options {
        Some(options) => vec![argv, options],
        None => vec![argv],
    };
    process_run::run(args, None)
}

fn record(argv: Value, options: Option<Value>) -> BTreeMap<String, Value> {
    match run(argv, options) {
        Ok(Value::Record(record)) => record,
        Ok(other) => panic!("proc_run answered {other:?} instead of a result record"),
        Err(error) => panic!("proc_run failed: {error}"),
    }
}

/// The message of the refusal `argv` must get. Nothing is started.
fn refusal(argv: Value, options: Option<Value>) -> String {
    match run(argv, options) {
        Err(error) => error.to_string(),
        Ok(value) => panic!("proc_run accepted a call it must refuse: {value:?}"),
    }
}

fn refusal_of_raw(args: Vec<Value>, input: Option<Value>) -> String {
    require_human_mode();
    match process_run::run(args, input) {
        Err(error) => error.to_string(),
        Ok(value) => panic!("proc_run accepted a call it must refuse: {value:?}"),
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

fn optional_int(record: &BTreeMap<String, Value>, key: &str) -> Option<i64> {
    match field(record, key) {
        Value::Int(number) => Some(*number),
        Value::Null => None,
        other => panic!("`{key}` is {other:?}, neither an Int nor null"),
    }
}

/// The bytes the stream record actually carries, decoded from its base64.
fn captured(stream: &BTreeMap<String, Value>) -> Vec<u8> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    let encoded = optional_text(stream, "base64").expect("base64 is a String in every mode");
    STANDARD
        .decode(encoded)
        .unwrap_or_else(|error| panic!("base64 is not base64: {error}"))
}

// ── the contract ────────────────────────────────────────────────────────

#[test]
fn utf8_text_and_bytes_survive_the_round_trip() {
    let record = record(fixture("fixture_utf8_both_streams"), None);
    assert!(flag(&record, "success"), "{record:?}");

    let process = record_of(&record, "process");
    assert!(flag(process, "success"), "{process:?}");
    assert_eq!(int(process, "exit_code"), 0);
    assert!(!flag(process, "timed_out"));
    assert!(!flag(process, "output_limit_exceeded"));
    assert!(flag(process, "cleanup_complete"), "{process:?}");
    assert!(
        flag(process, "group_termination_requested"),
        "the group is asked to stop on every path: {process:?}"
    );

    let stderr = record_of(&record, "stderr");
    assert_eq!(optional_text(stderr, "encoding").as_deref(), Some("utf-8"));
    assert_eq!(int(stderr, "bytes"), UTF8_PAYLOAD.len() as i64);
    assert_eq!(captured(stderr), UTF8_PAYLOAD.as_bytes());
    assert_eq!(optional_text(stderr, "text").as_deref(), Some(UTF8_PAYLOAD));
    assert_eq!(optional_flag(stderr, "decoded"), Some(true));
    assert!(flag(stderr, "complete"));
    assert_eq!(optional_text(stderr, "error"), None);

    // stdout carries libtest's own prologue, so only the payload is exact.
    let stdout = record_of(&record, "stdout");
    assert!(flag(stdout, "complete"), "{stdout:?}");
    assert_eq!(optional_flag(stdout, "decoded"), Some(true));
    let text = optional_text(stdout, "text").expect("stdout decodes");
    assert!(
        text.ends_with(UTF8_STDOUT),
        "stdout does not end with the payload: {text:?}"
    );
    assert!(
        captured(stdout).ends_with(UTF8_STDOUT.as_bytes()),
        "the captured bytes do not end with the payload"
    );
}

#[test]
fn a_child_that_finishes_immediately_reports_its_bytes_every_time() {
    // A reader can send its result and finish between the caller's drain of the
    // channel and its liveness check. Reading liveness before draining again
    // replaced that queued result with an empty-stream error, so the child's
    // bytes were reported missing even though they had been read. Back-to-back
    // runs of a child that writes and exits at once is the shape that exposes
    // it: there is deliberately no sleep and no timing assumption here.
    for attempt in 1..=20 {
        let record = record(fixture("fixture_utf8_both_streams"), None);
        let process = record_of(&record, "process");
        assert!(flag(process, "success"), "run {attempt}: {record:?}");
        assert!(
            flag(process, "cleanup_complete"),
            "run {attempt}: {process:?}"
        );

        let stderr = record_of(&record, "stderr");
        assert_eq!(
            optional_text(stderr, "error"),
            None,
            "run {attempt} lost the reader's report: {stderr:?}"
        );
        assert!(flag(stderr, "complete"), "run {attempt}: {stderr:?}");
        assert_eq!(captured(stderr), UTF8_PAYLOAD.as_bytes(), "run {attempt}");
        assert_eq!(
            optional_text(stderr, "text").as_deref(),
            Some(UTF8_PAYLOAD),
            "run {attempt}"
        );
    }
}

#[test]
fn bytes_mode_reports_the_bytes_and_no_decoding() {
    // GBK bytes would not decode as the default UTF-8; naming `bytes` says so
    // deliberately rather than by failing.
    let record = record(
        fixture("fixture_gbk"),
        Some(options(&[("encoding", Value::Str("bytes".into()))])),
    );
    assert!(flag(&record, "success"), "{record:?}");
    assert!(flag(record_of(&record, "process"), "success"));

    let stderr = record_of(&record, "stderr");
    assert_eq!(optional_text(stderr, "encoding").as_deref(), Some("bytes"));
    assert_eq!(int(stderr, "bytes"), GBK_BYTES.len() as i64);
    assert_eq!(captured(stderr), GBK_BYTES);
    assert_eq!(optional_text(stderr, "text"), None);
    assert_eq!(optional_flag(stderr, "decoded"), None);
    assert!(flag(stderr, "complete"));
    assert_eq!(optional_text(stderr, "error"), None);
}

#[test]
fn gbk_bytes_decode_only_when_gbk_is_named() {
    let gbk = record(
        fixture("fixture_gbk"),
        Some(options(&[("encoding", Value::Str("gbk".into()))])),
    );
    assert!(flag(&gbk, "success"), "{gbk:?}");
    let stderr = record_of(&gbk, "stderr");
    assert_eq!(optional_text(stderr, "text").as_deref(), Some(GBK_TEXT));
    assert_eq!(optional_flag(stderr, "decoded"), Some(true));
    assert_eq!(int(stderr, "bytes"), GBK_BYTES.len() as i64);
    assert_eq!(optional_text(stderr, "error"), None);

    // The same bytes decoded as UTF-8 are a decoding failure, not a process
    // failure: the exit status and the bytes both survive it.
    let utf8 = record(fixture("fixture_gbk"), None);
    let process = record_of(&utf8, "process");
    assert!(flag(process, "success"), "{process:?}");
    assert_eq!(int(process, "exit_code"), 0);
    assert!(!flag(&utf8, "success"), "{utf8:?}");

    let stderr = record_of(&utf8, "stderr");
    assert_eq!(optional_text(stderr, "encoding").as_deref(), Some("utf-8"));
    assert_eq!(optional_text(stderr, "text"), None);
    assert_eq!(optional_flag(stderr, "decoded"), Some(false));
    assert!(
        flag(stderr, "complete"),
        "a decode failure is not a read failure"
    );
    assert_eq!(int(stderr, "bytes"), GBK_BYTES.len() as i64);
    assert_eq!(
        captured(stderr),
        GBK_BYTES,
        "the raw bytes must be unchanged"
    );
    let error = optional_text(stderr, "error").expect("the refusal must say what failed");
    assert!(
        error.contains("UTF-8"),
        "the error must name the codec: {error}"
    );
}

#[test]
fn a_utf16_byte_order_mark_is_part_of_the_stream_in_both_orders() {
    for (fixture_name, encoding, bytes) in [
        ("fixture_utf16le_bom", "utf-16le", UTF16LE_BOM_BYTES),
        ("fixture_utf16be_bom", "utf-16be", UTF16BE_BOM_BYTES),
    ] {
        let record = record(
            fixture(fixture_name),
            Some(options(&[("encoding", Value::Str(encoding.into()))])),
        );
        assert!(flag(&record, "success"), "{fixture_name}: {record:?}");
        let stderr = record_of(&record, "stderr");
        assert_eq!(
            optional_text(stderr, "text").as_deref(),
            Some(UTF16_BOM_TEXT),
            "{fixture_name}: the mark must decode to U+FEFF, not be stripped"
        );
        assert_eq!(optional_flag(stderr, "decoded"), Some(true));
        assert_eq!(captured(stderr), bytes);
    }
}

#[test]
fn invalid_bytes_keep_the_exit_status_and_the_raw_data() {
    let record = record(fixture("fixture_invalid_utf8"), None);
    let process = record_of(&record, "process");
    assert!(flag(process, "success"), "{process:?}");
    assert_eq!(int(process, "exit_code"), 0);
    assert!(flag(process, "cleanup_complete"));
    assert!(!flag(&record, "success"), "a failed decode is not success");

    let stderr = record_of(&record, "stderr");
    assert_eq!(optional_flag(stderr, "decoded"), Some(false));
    assert_eq!(optional_text(stderr, "text"), None);
    assert_eq!(captured(stderr), INVALID_BYTES);
    assert!(optional_text(stderr, "error").is_some());
}

#[test]
fn a_nonzero_exit_is_a_fact_and_not_an_error() {
    let record = record(fixture("fixture_exit_code_three"), None);
    assert!(!flag(&record, "success"), "{record:?}");

    let process = record_of(&record, "process");
    assert!(!flag(process, "success"));
    assert_eq!(int(process, "exit_code"), 3);
    assert!(!flag(process, "timed_out"));
    assert!(!flag(process, "output_limit_exceeded"));
    assert!(flag(process, "cleanup_complete"), "{process:?}");
    assert!(flag(process, "group_termination_requested"), "{process:?}");

    let stderr = record_of(&record, "stderr");
    assert_eq!(optional_text(stderr, "text").as_deref(), Some("boom\n"));
    assert!(flag(stderr, "complete"));
    assert_eq!(optional_text(stderr, "error"), None);
    assert_eq!(captured(stderr).as_slice(), b"boom\n");
}

#[test]
fn output_past_the_limit_is_truncated_and_says_so() {
    let record = record(
        fixture("fixture_noisy"),
        Some(options(&[("max_output_bytes", Value::Int(1024))])),
    );
    assert!(
        !flag(&record, "success"),
        "a truncated capture is not success"
    );

    let process = record_of(&record, "process");
    assert!(flag(process, "output_limit_exceeded"), "{process:?}");
    assert!(!flag(process, "timed_out"));
    assert!(flag(process, "cleanup_complete"), "{process:?}");
    assert!(flag(process, "group_termination_requested"), "{process:?}");

    let stderr = record_of(&record, "stderr");
    assert_eq!(int(stderr, "bytes"), 1024, "the capture stops at the limit");
    assert_eq!(captured(stderr), vec![b'z'; 1024]);
    let expected = "z".repeat(1024);
    assert_eq!(
        optional_text(stderr, "text").as_deref(),
        Some(expected.as_str())
    );
    assert!(!flag(stderr, "complete"));
    let error = optional_text(stderr, "error").expect("a partial capture must say so");
    assert!(
        error.contains("only the first"),
        "the error must not claim the total: {error}"
    );

    // The stream that stayed under the limit is untouched.
    assert!(flag(record_of(&record, "stdout"), "complete"));
}

#[test]
fn a_timeout_requests_group_termination_and_reports_the_acknowledgement() {
    let record = record(
        fixture("fixture_sleeps"),
        Some(options(&[("timeout_ms", Value::Int(2000))])),
    );
    assert!(!flag(&record, "success"), "{record:?}");

    let process = record_of(&record, "process");
    assert!(flag(process, "timed_out"), "{process:?}");
    assert!(flag(process, "cleanup_complete"), "{process:?}");
    assert!(
        flag(process, "group_termination_requested"),
        "a timeout must ask the group to stop: {process:?}"
    );
    assert!(!flag(process, "success"));
    assert_ne!(
        optional_int(process, "exit_code"),
        Some(0),
        "a terminated process must not report a clean exit: {process:?}"
    );

    let stderr = record_of(&record, "stderr");
    assert_eq!(optional_text(stderr, "text").as_deref(), Some("awake\n"));
    assert!(
        flag(stderr, "complete"),
        "an EOF here means the process that held this pipe exited"
    );
}

#[test]
fn a_descendant_holding_the_pipes_is_still_bounded_by_the_deadline() {
    // The leader exits at once and a descendant keeps both pipes open. The
    // captures cannot close, so `timed_out` is the proof that the descendant
    // really was holding them; the acknowledgement shows the group was asked to
    // stop, and both captures closing means whoever held the pipes exited.
    // Neither flag claims the host has no process left — nothing here can prove
    // that.
    let started = Instant::now();
    let record = record(
        fixture("fixture_tree"),
        Some(options(&[("timeout_ms", Value::Int(2000))])),
    );
    let elapsed = started.elapsed();

    let process = record_of(&record, "process");
    assert!(flag(process, "success"), "the leader exited 0: {process:?}");
    assert_eq!(int(process, "exit_code"), 0);
    assert!(flag(process, "timed_out"), "{process:?}");
    assert!(flag(process, "group_termination_requested"), "{process:?}");
    assert!(flag(process, "cleanup_complete"), "{process:?}");
    assert!(!flag(&record, "success"), "{record:?}");
    assert!(flag(record_of(&record, "stderr"), "complete"));
    assert!(flag(record_of(&record, "stdout"), "complete"));
    assert_eq!(
        optional_text(record_of(&record, "stderr"), "text").as_deref(),
        Some("held\n")
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "the call must return at the deadline, took {elapsed:?}"
    );
}

#[test]
fn the_program_and_its_arguments_are_passed_literally() {
    let record = record(
        fixture_with_value("fixture_echo_argument", SHELL_PAYLOAD),
        None,
    );
    assert!(flag(&record, "success"), "{record:?}");

    let stderr = record_of(&record, "stderr");
    assert_eq!(
        optional_text(stderr, "text").as_deref(),
        Some(SHELL_PAYLOAD),
        "the argument did not arrive as written"
    );
    assert_eq!(captured(stderr), SHELL_PAYLOAD.as_bytes());

    let stdout = record_of(&record, "stdout");
    let text = optional_text(stdout, "text").expect("stdout decodes");
    assert!(
        text.ends_with(SHELL_PAYLOAD),
        "stdout does not end with the argument: {text:?}"
    );
}

#[test]
fn every_option_is_validated_before_anything_is_started() {
    // A rejected call must not have started anything. The fixture writes a
    // marker, so a spawn would be visible even though the call still refused.
    let marker = std::env::temp_dir().join(format!("ae_proc_run_marker_{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let argv = fixture_with_value("fixture_marker", &marker.to_string_lossy());

    let error = refusal(argv, Some(options(&[("unknown", Value::Int(1))])));
    assert!(error.contains("E_BAD_ARG"), "not coded: {error}");
    assert!(
        !marker.exists(),
        "the program ran even though the option was refused"
    );

    // The same shape with valid options does start it, so the assertion above
    // is about validation and not about a fixture that never writes.
    let ok = record(
        fixture_with_value("fixture_marker", &marker.to_string_lossy()),
        None,
    );
    assert!(flag(&ok, "success"), "{ok:?}");
    assert!(marker.exists(), "the marker fixture did not run");
    let _ = std::fs::remove_file(&marker);
}

#[test]
fn refusing_a_call_names_the_rule_it_broke() {
    let program = fixture("fixture_echo_argument");

    for (options, why) in [
        (
            options(&[("encoding", Value::Str("latin-1".into()))]),
            "unknown encoding",
        ),
        (
            options(&[("encoding", Value::Int(8))]),
            "encoding is not a String",
        ),
        (options(&[("timeout_ms", Value::Int(0))]), "zero timeout"),
        (
            options(&[("timeout_ms", Value::Int(-1))]),
            "negative timeout",
        ),
        (
            options(&[("timeout_ms", Value::Int(600_001))]),
            "timeout past the cap",
        ),
        (
            options(&[("timeout_ms", Value::Str("100".into()))]),
            "timeout is not an Int",
        ),
        (
            options(&[("timeout_ms", Value::Float(1.5))]),
            "timeout is not an Int",
        ),
        (
            options(&[("max_output_bytes", Value::Int(0))]),
            "zero limit",
        ),
        (
            options(&[("max_output_bytes", Value::Int(16_777_217))]),
            "limit past the cap",
        ),
        (
            options(&[("max_output_bytes", Value::Null)]),
            "limit is not an Int",
        ),
    ] {
        let error = refusal(program.clone(), Some(options));
        assert!(error.contains("E_BAD_ARG"), "{why}: {error}");
    }

    // Wrong argv shapes, and the argument that cannot cross a process boundary.
    for (argv, why) in [
        (Value::Array(vec![]), "no program"),
        (
            Value::Array(vec![Value::Str(String::new())]),
            "an empty program",
        ),
        (Value::Array(vec![Value::Int(7)]), "a non-String program"),
        (
            Value::Array(vec![Value::Str("ok".into()), Value::Int(7)]),
            "a non-String argument",
        ),
        (
            Value::Array(vec![Value::Str("a\0b".into())]),
            "a NUL in the program",
        ),
        (
            Value::Array(vec![Value::Str("ok".into()), Value::Str("a\0b".into())]),
            "a NUL in an argument",
        ),
        (Value::Str("not an array".into()), "argv is not an array"),
        (Value::Null, "argv is null"),
    ] {
        let error = refusal(argv, None);
        assert!(error.contains("E_BAD_ARG"), "{why}: {error}");
    }

    // A third positional argument, a non-Record options value, and a piped
    // subject: all refused, all coded.
    let error = refusal_of_raw(vec![program.clone(), options(&[]), options(&[])], None);
    assert!(error.contains("E_BAD_ARG"), "three arguments: {error}");

    let error = refusal_of_raw(vec![program.clone(), Value::Array(vec![])], None);
    assert!(
        error.contains("E_BAD_ARG"),
        "options is not a Record: {error}"
    );

    let error = refusal_of_raw(vec![program], Some(Value::Str("piped".into())));
    assert!(error.contains("E_BAD_ARG"), "piped input: {error}");
    assert!(
        error.contains("piped"),
        "the refusal must say the pipe is not read: {error}"
    );
}

#[test]
fn the_largest_legal_bounds_are_accepted() {
    // The cap is a bound, not a threshold: the largest legal value is accepted,
    // and the values just past it are refused by the test above.
    let record = record(
        fixture("fixture_echo_argument"),
        Some(options(&[
            ("timeout_ms", Value::Int(600_000)),
            ("max_output_bytes", Value::Int(16_777_216)),
        ])),
    );
    assert!(flag(&record, "success"), "{record:?}");
}

#[test]
fn a_program_that_cannot_be_started_is_a_coded_error() {
    let error = refusal(
        Value::Array(vec![Value::Str("ae_no_such_program_94117".into())]),
        None,
    );
    assert!(error.contains("E_TOOL_MISSING"), "not coded: {error}");
    assert!(
        error.contains("ae_no_such_program_94117"),
        "the refusal must name the program: {error}"
    );
}

#[test]
fn the_builtin_is_reachable_through_the_dispatcher() {
    // `process_run::run` is the module's own entry point; this pins that the
    // dispatcher reaches it too, with the declaration's argument shapes.
    require_human_mode();
    let result = aethershell::builtins::call(
        "proc_run",
        vec![
            fixture("fixture_exit_code_three"),
            options(&[("encoding", Value::Str("bytes".into()))]),
        ],
        &mut Env::new(),
    );

    match result {
        Ok(Value::Record(record)) => {
            let process = record_of(&record, "process");
            assert_eq!(int(process, "exit_code"), 3, "{record:?}");
            assert!(!flag(process, "success"));
            let stderr = record_of(&record, "stderr");
            assert_eq!(captured(stderr).as_slice(), b"boom\n");
            assert_eq!(optional_text(stderr, "encoding").as_deref(), Some("bytes"));
        }
        // A host in agent mode refuses `Exec` before the module sees the call,
        // which is the gate working, not this module failing.
        Ok(other) => panic!("proc_run answered {other:?} instead of a record"),
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains("E_POLICY_DENY") || message.contains("E_NEEDS_APPROVAL"),
                "the dispatcher refused the call for a reason that is not the policy: {message}"
            );
        }
    }
}

#[test]
fn agent_mode_gates_a_direct_call_and_starts_nothing() {
    // `proc_run` is `SELF_GUARDED`, so the dispatcher does not gate it: the gate
    // inside `run` is the only one there is, and a direct call must reach it.
    // Agent mode is set on this child's own environment instead of the test
    // process's, because `AETHER_*` is process-global and the existing gate
    // tests already own that lock.
    let dir = std::env::temp_dir().join(format!("ae_proc_run_agent_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the private directory");
    let marker = dir.join("started.txt");
    let _ = std::fs::remove_file(&marker);

    let output = Command::new(executable())
        .args([
            "--exact",
            "fixture_agent_guard",
            "--ignored",
            "--nocapture",
            "--skip",
        ])
        .arg(marker.to_string_lossy().as_ref())
        .env("AETHER_MODE", "agent")
        .env("AETHER_POLICY", "strict")
        .env("AETHER_WORKSPACE", &dir)
        .env("AETHER_AUDIT_LOG", dir.join("audit.log"))
        .env_remove("AETHER_APPROVE")
        .env_remove("AETHER_APPROVE_ALL")
        .output()
        .expect("run the agent-mode probe");

    let reported = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        reported.contains("answer=err"),
        "a direct call ran in agent mode: {reported}"
    );
    assert!(
        reported.contains("E_NEEDS_APPROVAL") || reported.contains("E_POLICY_DENY"),
        "the refusal is not the policy's: {reported}"
    );
    assert!(
        reported.contains("marker=false") && !marker.exists(),
        "the program was started in agent mode: {reported}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

//! `proc_run` options end to end: the child's directory, environment and stdin,
//! the tail capture, the continue-on-limit choice, and the log files.
//!
//! Every process this file starts is this test binary itself, run with
//! `--exact child_probe --ignored --nocapture`. One fixture serves every case
//! and is told what to do by the environment `proc_run` gives it, so the whole
//! file needs no host program and no second compiled artifact. The fixture is
//! `#[ignore]`d, so a plain `cargo test` runs none of it.
//!
//! libtest writes its own prologue to the child's **stdout** and nothing to its
//! stderr, so byte-exact assertions are made against the stderr stream and the
//! stdout assertions accept the prologue as a prefix. The fixture writes its
//! payload and then exits directly, which is also why it flushes explicitly:
//! `process::exit` runs no destructors.
//!
//! Nothing here changes the test process's own environment or working
//! directory: every value the child sees is supplied per call through the
//! `env` and `cwd` options.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aethershell::process_run;
use aethershell::safety;
use aethershell::value::Value;

// ── the fixture's own control values, all supplied through `env` ────────

const MODE: &str = "FORK_PROC_CHILD_MODE";
const VALUE: &str = "FORK_PROC_CHILD_VALUE";
const READ: &str = "FORK_PROC_CHILD_READ";
const MARKER: &str = "FORK_PROC_CHILD_MARKER";
const WRITE: &str = "FORK_PROC_CHILD_WRITE";
const SLEEP_MS: &str = "FORK_PROC_CHILD_SLEEP_MS";

/// A payload with a character outside ASCII, so a byte-exact comparison also
/// proves nothing re-encoded the stream.
const PAYLOAD: &str = "héllo — ✓ tail";

/// How many bytes the noisy fixtures write, far past any limit used here.
const NOISY_BYTES: i64 = 200_000;

// ── the fixture: only ever started by a test that passes `--ignored` ────

#[test]
#[ignore]
fn child_probe() {
    let mode = std::env::var(MODE).unwrap_or_else(|_| "none".to_string());
    match mode.as_str() {
        // The directory the child was started in.
        "cwd" => {
            let dir = std::env::current_dir().expect("the child has a working directory");
            write_stderr(dir.to_string_lossy().as_bytes());
        }
        // The value the caller put in the child's environment.
        "echo-env" => {
            let value = std::env::var(VALUE).unwrap_or_else(|_| "<unset>".to_string());
            write_stderr(value.as_bytes());
        }
        // Whether one named variable is set for the child.
        "read-env" => {
            let name = std::env::var(READ).unwrap_or_default();
            let text = match std::env::var(&name) {
                Ok(value) => format!("set:{value}"),
                Err(_) => "unset".to_string(),
            };
            write_stderr(text.as_bytes());
        }
        // Everything the caller sent on stdin, echoed back byte for byte.
        "echo-stdin" => {
            use std::io::Read;

            let mut bytes = Vec::new();
            std::io::stdin()
                .read_to_end(&mut bytes)
                .expect("read the child's stdin");
            write_stderr(&bytes);
        }
        // The payload on both streams.
        "payload" => {
            let value = std::env::var(VALUE).unwrap_or_default();
            write_stdout(value.as_bytes());
            write_stderr(value.as_bytes());
        }
        // A file at the path the caller named, so a rejected call is visible.
        "marker" => {
            if let Ok(path) = std::env::var(MARKER) {
                std::fs::write(path, b"started").expect("write the marker");
            }
        }
        // A stream far past the caller's limit, then a pause. Under the default
        // the group is asked to stop during the pause; under `continue` the
        // child finishes it.
        "noisy-then-sleep" => {
            write_noisy(read_int(WRITE));
            std::thread::sleep(Duration::from_millis(read_int(SLEEP_MS).max(1) as u64));
        }
        // A long child that never reads its stdin.
        "sleep-ignore-stdin" => {
            write_stderr(b"awake\n");
            std::thread::sleep(Duration::from_millis(read_int(SLEEP_MS).max(1) as u64));
        }
        other => write_stderr(format!("unknown mode {other}").as_bytes()),
    }
    std::process::exit(0);
}

/// Writes `bytes` to stderr without capturing more than the caller asked for.
fn write_noisy(bytes: i64) {
    use std::io::Write;

    let chunk = [b'z'; 4096];
    let mut stderr = std::io::stderr().lock();
    let mut written = 0i64;
    while written < bytes {
        let take = (bytes - written).min(chunk.len() as i64) as usize;
        stderr.write_all(&chunk[..take]).expect("write to stderr");
        written += take as i64;
    }
    stderr.flush().expect("flush stderr");
}

fn read_int(name: &str) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
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
/// tests are about the module, not the gate, so a host that selected an agent
/// policy is stated as a precondition: this suite never changes the environment
/// it was started with.
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

/// The argv that makes this test binary run the one ignored fixture.
fn argv() -> Value {
    Value::Array(
        [
            executable(),
            "--exact".to_string(),
            "child_probe".to_string(),
            "--ignored".to_string(),
            "--nocapture".to_string(),
        ]
        .into_iter()
        .map(Value::Str)
        .collect(),
    )
}

fn options(pairs: Vec<(&str, Value)>) -> Value {
    Value::Record(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

/// Options that start the fixture in one mode, with any extra control values.
///
/// Every control value travels through `env`, so the argv is identical for every
/// case and nothing here depends on a host program.
fn child(mode: &str, extra: &[(&str, Option<&str>)]) -> Vec<(&'static str, Value)> {
    let mut environment = BTreeMap::new();
    environment.insert(MODE.to_string(), Value::Str(mode.to_string()));
    for (name, value) in extra {
        environment.insert(
            (*name).to_string(),
            match value {
                Some(value) => Value::Str((*value).to_string()),
                None => Value::Null,
            },
        );
    }
    vec![("env", Value::Record(environment))]
}

fn run_with(argv: Value, options: Option<Value>) -> anyhow::Result<Value> {
    require_human_mode();
    let mut args = vec![argv];
    if let Some(options) = options {
        args.push(options);
    }
    process_run::run(args, None)
}

fn run(options: Option<Value>) -> anyhow::Result<Value> {
    run_with(argv(), options)
}

fn record(options: Option<Value>) -> BTreeMap<String, Value> {
    match run(options) {
        Ok(Value::Record(record)) => record,
        Ok(other) => panic!("proc_run answered {other:?} instead of a result record"),
        Err(error) => panic!("proc_run failed: {error}"),
    }
}

/// The message of the refusal this call must get. Nothing is started.
fn refusal(options: Option<Value>) -> String {
    refusal_with(argv(), options)
}

fn refusal_with(argv: Value, options: Option<Value>) -> String {
    match run_with(argv, options) {
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

fn optional_text(record: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    match field(record, key) {
        Value::Str(value) => Some(value.clone()),
        Value::Null => None,
        other => panic!("`{key}` is {other:?}, neither a String nor null"),
    }
}

fn text(record: &BTreeMap<String, Value>, key: &str) -> String {
    optional_text(record, key).unwrap_or_else(|| panic!("`{key}` is null in {record:?}"))
}

/// The bytes a stream record actually carries, decoded from its base64.
fn captured(stream: &BTreeMap<String, Value>) -> Vec<u8> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    let encoded = optional_text(stream, "base64").expect("base64 is a String in every mode");
    STANDARD
        .decode(encoded)
        .unwrap_or_else(|error| panic!("base64 is not base64: {error}"))
}

/// The reported bytes of the child's stdin after a call.
fn stdin_report(record: &BTreeMap<String, Value>) -> &BTreeMap<String, Value> {
    record_of(record, "stdin")
}

// ── a private directory per test ────────────────────────────────────────

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "ae_proc_run_opts_{}_{label}_{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create the private directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn as_str(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ── the child's directory ───────────────────────────────────────────────

#[test]
fn the_child_runs_in_the_requested_working_directory() {
    let dir = TempDir::new("cwd");
    let before = std::env::current_dir().expect("the interpreter's directory");

    let mut pairs = child("cwd", &[]);
    pairs.insert(0, ("cwd", Value::Str(dir.as_str())));
    let result = record(Some(options(pairs)));
    assert!(flag(&result, "success"), "{result:?}");

    let reported = text(record_of(&result, "stderr"), "text");
    let reported = PathBuf::from(reported.trim_end_matches(['\n', '\r']));
    assert_eq!(
        reported.canonicalize().expect("the reported directory"),
        dir.path().canonicalize().expect("the created directory"),
        "the child did not run in the directory that was asked for"
    );
    assert_eq!(
        std::env::current_dir().expect("the interpreter's directory"),
        before,
        "the interpreter's own working directory must never move"
    );
}

#[test]
fn a_cwd_that_does_not_exist_is_refused_and_nothing_starts() {
    let dir = TempDir::new("missing-cwd");
    let missing = dir.join("no-such-directory");
    let marker = dir.join("started.txt");

    let mut pairs = child(
        "marker",
        &[(MARKER, Some(marker.to_str().expect("a UTF-8 marker path")))],
    );
    pairs.insert(
        0,
        ("cwd", Value::Str(missing.to_string_lossy().into_owned())),
    );

    let error = refusal(Some(options(pairs)));
    assert!(error.contains("E_BAD_ARG"), "not coded: {error}");
    assert!(
        !marker.exists(),
        "the program ran even though its working directory was refused"
    );
}

// ── the child's environment ─────────────────────────────────────────────

#[test]
fn the_child_environment_is_overridden_without_touching_the_interpreter() {
    assert!(
        std::env::var(VALUE).is_err(),
        "the precondition is that this name is absent from the suite's own environment"
    );
    let mut pairs = child("echo-env", &[(VALUE, Some("child-only-value"))]);
    pairs.push(("encoding", Value::Str("bytes".into())));

    let result = record(Some(options(pairs)));
    assert_eq!(
        captured(record_of(&result, "stderr")),
        b"child-only-value".to_vec()
    );
    assert!(
        std::env::var(VALUE).is_err(),
        "an env option must not reach the interpreter's own environment"
    );
}

#[test]
fn a_null_env_value_removes_the_name_from_the_child_only() {
    assert!(
        std::env::var_os("PATH").is_some(),
        "the precondition is that PATH is set for this suite"
    );

    let mut pairs = child("read-env", &[(READ, Some("PATH")), ("PATH", None)]);
    pairs.push(("encoding", Value::Str("bytes".into())));

    let result = record(Some(options(pairs)));
    assert_eq!(
        captured(record_of(&result, "stderr")),
        b"unset".to_vec(),
        "a null env value must remove the name from the child"
    );
    assert!(
        std::env::var_os("PATH").is_some(),
        "the removal must not reach the interpreter's own environment"
    );
}

#[test]
fn managed_environment_namespaces_are_refused() {
    for name in ["AETHER_MODE", "AE_POLICY", "DSH_TOKEN", "aether_mode"] {
        let dir = TempDir::new("managed-env");
        let marker = dir.join("started.txt");
        let mut environment = BTreeMap::new();
        environment.insert(MODE.to_string(), Value::Str("marker".into()));
        environment.insert(
            MARKER.to_string(),
            Value::Str(marker.to_string_lossy().into_owned()),
        );
        environment.insert(name.to_string(), Value::Str("x".into()));

        let error = refusal(Some(options(vec![("env", Value::Record(environment))])));
        assert!(error.contains("E_BAD_ARG"), "{name}: {error}");
        assert!(!marker.exists(), "{name}: the program ran anyway");
    }
}

#[cfg(windows)]
#[test]
fn env_names_that_differ_only_by_case_are_refused_on_windows() {
    let mut environment = BTreeMap::new();
    environment.insert("Fork_Proc_Case".to_string(), Value::Str("a".into()));
    environment.insert("FORK_PROC_CASE".to_string(), Value::Str("b".into()));

    let error = refusal(Some(options(vec![("env", Value::Record(environment))])));
    assert!(error.contains("E_BAD_ARG"), "not coded: {error}");
}

// ── stdin ───────────────────────────────────────────────────────────────

#[test]
fn stdin_text_reaches_the_child_exactly() {
    let mut pairs = child("echo-stdin", &[]);
    pairs.push(("stdin", Value::Str("no trailing newline".into())));
    pairs.push(("encoding", Value::Str("bytes".into())));

    let result = record(Some(options(pairs)));
    let stdin = stdin_report(&result);
    assert!(flag(stdin, "requested"), "{stdin:?}");
    assert_eq!(int(stdin, "bytes"), 19);
    assert_eq!(int(stdin, "written_bytes"), 19, "{stdin:?}");
    assert!(flag(stdin, "complete"), "{stdin:?}");
    assert_eq!(optional_text(stdin, "error"), None, "{stdin:?}");
    assert_eq!(
        captured(record_of(&result, "stderr")),
        b"no trailing newline".to_vec(),
        "the child did not receive exactly the bytes that were sent"
    );
}

#[test]
fn stdin_base64_reaches_the_child_as_exact_bytes() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    // A NUL, a value that is not valid UTF-8, and text around them.
    let bytes: Vec<u8> = vec![0x00, 0xFF, 0x41, 0x80, 0x0A];
    let mut pairs = child("echo-stdin", &[]);
    pairs.push(("stdin_base64", Value::Str(STANDARD.encode(&bytes))));
    pairs.push(("encoding", Value::Str("bytes".into())));

    let result = record(Some(options(pairs)));
    let stdin = stdin_report(&result);
    assert_eq!(int(stdin, "bytes"), bytes.len() as i64);
    assert!(flag(stdin, "complete"), "{stdin:?}");
    assert_eq!(
        captured(record_of(&result, "stderr")),
        bytes,
        "the arbitrary bytes did not survive the round trip"
    );
}

#[test]
fn stdin_and_stdin_base64_together_are_refused() {
    let error = refusal(Some(options(vec![
        ("stdin", Value::Str("one".into())),
        ("stdin_base64", Value::Str("b25l".into())),
    ])));
    assert!(error.contains("E_BAD_ARG"), "not coded: {error}");
}

#[test]
fn a_bad_base64_stdin_is_refused_and_nothing_starts() {
    let dir = TempDir::new("bad-stdin");
    let marker = dir.join("started.txt");
    let mut pairs = child(
        "marker",
        &[(MARKER, Some(marker.to_str().expect("a UTF-8 marker path")))],
    );
    pairs.push(("stdin_base64", Value::Str("not base64!".into())));

    let error = refusal(Some(options(pairs)));
    assert!(error.contains("E_BAD_ARG"), "not coded: {error}");
    assert!(!marker.exists(), "the program ran with an invalid stdin");
}

#[test]
fn a_large_stdin_flows_while_both_streams_are_read() {
    // Past any adapter budget and past what a pipe buffer holds, so the writer
    // must run concurrently with the reader or this call would deadlock.
    let payload = "s".repeat(2 * 1024 * 1024);
    let mut pairs = child("echo-stdin", &[]);
    pairs.push(("stdin", Value::Str(payload.clone())));
    pairs.push(("max_output_bytes", Value::Int(4_194_304)));
    pairs.push(("encoding", Value::Str("bytes".into())));

    let started = Instant::now();
    let result = record(Some(options(pairs)));
    let elapsed = started.elapsed();

    assert!(flag(&result, "success"), "{result:?}");
    assert_eq!(
        captured(record_of(&result, "stderr")),
        payload.as_bytes().to_vec()
    );
    assert!(
        flag(stdin_report(&result), "complete"),
        "the writer did not finish"
    );
    assert!(elapsed < Duration::from_secs(60), "took {elapsed:?}");
}

#[test]
fn the_stdin_record_is_present_and_honest_without_stdin() {
    let result = record(Some(options(child("payload", &[(VALUE, Some("x"))]))));
    let stdin = stdin_report(&result);
    assert!(!flag(stdin, "requested"), "{stdin:?}");
    assert_eq!(int(stdin, "bytes"), 0, "{stdin:?}");
    assert_eq!(int(stdin, "written_bytes"), 0, "{stdin:?}");
    assert!(flag(stdin, "complete"), "{stdin:?}");
    assert_eq!(optional_text(stdin, "error"), None, "{stdin:?}");
}

// ── the capture mode and the output limit ───────────────────────────────

#[test]
fn capture_tail_keeps_the_end_of_the_stream() {
    let payload = "0123456789";
    let mut pairs = child("payload", &[(VALUE, Some(payload))]);
    pairs.push(("capture", Value::Str("tail".into())));
    pairs.push(("max_output_bytes", Value::Int(4)));
    pairs.push(("encoding", Value::Str("bytes".into())));

    let result = record(Some(options(pairs)));
    assert!(
        flag(record_of(&result, "process"), "success"),
        "the child exited zero: {result:?}"
    );
    assert!(
        !flag(&result, "success"),
        "a partial capture is not success"
    );

    let stderr = record_of(&result, "stderr");
    assert_eq!(optional_text(stderr, "capture").as_deref(), Some("tail"));
    assert_eq!(captured(stderr), b"6789".to_vec());
    assert_eq!(int(stderr, "bytes"), 4);
    assert_eq!(int(stderr, "total_bytes"), 10);
    assert_eq!(int(stderr, "offset_bytes"), 6);
    assert!(!flag(stderr, "complete"));
    assert!(flag(record_of(&result, "process"), "output_limit_exceeded"));
}

#[test]
fn continue_past_the_limit_lets_the_child_finish() {
    let bytes = NOISY_BYTES.to_string();
    let mut pairs = child(
        "noisy-then-sleep",
        &[(WRITE, Some(bytes.as_str())), (SLEEP_MS, Some("2500"))],
    );
    pairs.push(("max_output_bytes", Value::Int(1024)));
    pairs.push(("on_output_limit", Value::Str("continue".into())));

    let started = Instant::now();
    let result = record(Some(options(pairs)));
    let elapsed = started.elapsed();

    let process = record_of(&result, "process");
    assert!(
        flag(process, "success"),
        "continue must let the child finish: {process:?}"
    );
    assert_eq!(int(process, "exit_code"), 0, "{process:?}");
    assert!(!flag(process, "timed_out"), "{process:?}");
    assert!(flag(process, "output_limit_exceeded"), "{process:?}");
    assert_eq!(
        int(record_of(&result, "stderr"), "total_bytes"),
        NOISY_BYTES
    );
    assert!(!flag(&result, "success"), "{result:?}");
    assert!(
        elapsed >= Duration::from_millis(2000),
        "the child cannot have finished its pause in {elapsed:?}"
    );
}

#[test]
fn the_default_asks_the_group_to_stop_at_the_limit() {
    let bytes = NOISY_BYTES.to_string();
    let mut pairs = child(
        "noisy-then-sleep",
        &[(WRITE, Some(bytes.as_str())), (SLEEP_MS, Some("2500"))],
    );
    pairs.push(("max_output_bytes", Value::Int(1024)));

    let started = Instant::now();
    let result = record(Some(options(pairs)));
    let elapsed = started.elapsed();

    let process = record_of(&result, "process");
    assert!(flag(process, "output_limit_exceeded"), "{process:?}");
    assert!(
        !flag(process, "success"),
        "the default stops the child at the limit: {process:?}"
    );
    assert!(!flag(process, "timed_out"), "{process:?}");
    assert!(flag(process, "group_termination_requested"), "{process:?}");
    assert!(!flag(&result, "success"), "{result:?}");
    assert!(
        elapsed < Duration::from_secs(20),
        "the call must return at the limit, took {elapsed:?}"
    );
}

// ── log files ───────────────────────────────────────────────────────────

#[test]
fn logs_receive_the_raw_bytes_and_report_their_size() {
    let dir = TempDir::new("logs");
    let out = dir.join("out.log");
    let err = dir.join("err.log");
    let mut pairs = child("payload", &[(VALUE, Some(PAYLOAD))]);
    pairs.push(("stdout_log", Value::Str(out.to_string_lossy().into_owned())));
    pairs.push(("stderr_log", Value::Str(err.to_string_lossy().into_owned())));

    let result = record(Some(options(pairs)));
    assert!(flag(&result, "success"), "{result:?}");

    let stderr = record_of(&result, "stderr");
    let log = record_of(stderr, "log");
    assert!(flag(log, "complete"), "{log:?}");
    assert_eq!(int(log, "bytes"), PAYLOAD.len() as i64);
    assert_eq!(optional_text(log, "error"), None, "{log:?}");
    assert_eq!(
        optional_text(log, "path").as_deref(),
        Some(err.to_string_lossy().as_ref())
    );
    assert_eq!(
        std::fs::read(&err).expect("read the stderr log"),
        PAYLOAD.as_bytes().to_vec(),
        "the log must hold the raw bytes, with no encoding or newline added"
    );

    // libtest writes its prologue to the child's stdout, so the payload is a
    // suffix there rather than the whole file.
    let stdout_log = std::fs::read(&out).expect("read the stdout log");
    assert!(
        stdout_log.ends_with(PAYLOAD.as_bytes()),
        "the stdout log does not end with the payload"
    );
    assert_eq!(
        int(record_of(&result, "stdout"), "total_bytes"),
        stdout_log.len() as i64
    );
}

#[test]
fn an_existing_log_file_is_refused_and_the_child_never_starts() {
    let dir = TempDir::new("existing-log");
    let log = dir.join("out.log");
    std::fs::write(&log, b"sentinel").expect("write the sentinel");
    let marker = dir.join("started.txt");

    let mut pairs = child(
        "marker",
        &[(MARKER, Some(marker.to_str().expect("a UTF-8 marker path")))],
    );
    pairs.push(("stdout_log", Value::Str(log.to_string_lossy().into_owned())));

    let error = refusal(Some(options(pairs)));
    assert!(error.contains("E_BAD_ARG"), "not coded: {error}");
    assert!(
        !marker.exists(),
        "the program ran with an unusable log path"
    );
    assert_eq!(
        std::fs::read(&log).expect("read the sentinel"),
        b"sentinel".to_vec(),
        "an existing file must never be overwritten"
    );
}

#[test]
fn both_logs_must_name_different_files() {
    let dir = TempDir::new("same-log");
    let log = dir.join("same.log").to_string_lossy().into_owned();

    let error = refusal(Some(options(vec![
        ("stdout_log", Value::Str(log.clone())),
        ("stderr_log", Value::Str(log)),
    ])));
    assert!(error.contains("E_BAD_ARG"), "not coded: {error}");
}

#[test]
fn a_log_in_a_directory_that_does_not_exist_is_refused() {
    let dir = TempDir::new("missing-log-dir");
    let log = dir.join("nowhere").join("out.log");
    let marker = dir.join("started.txt");

    let mut pairs = child(
        "marker",
        &[(MARKER, Some(marker.to_str().expect("a UTF-8 marker path")))],
    );
    pairs.push(("stderr_log", Value::Str(log.to_string_lossy().into_owned())));

    let error = refusal(Some(options(pairs)));
    assert!(error.contains("E_BAD_ARG"), "not coded: {error}");
    assert!(
        !marker.exists(),
        "the program ran with an unusable log path"
    );
    assert!(!log.exists(), "no directory should have been created");
}

#[test]
fn a_log_that_reaches_its_cap_is_reported_incomplete() {
    let dir = TempDir::new("log-cap");
    let log = dir.join("err.log");
    let bytes = NOISY_BYTES.to_string();
    let mut pairs = child(
        "noisy-then-sleep",
        &[(WRITE, Some(bytes.as_str())), (SLEEP_MS, Some("2500"))],
    );
    pairs.push(("stderr_log", Value::Str(log.to_string_lossy().into_owned())));
    pairs.push(("max_log_bytes", Value::Int(1024)));

    let result = record(Some(options(pairs)));
    let record = record_of(record_of(&result, "stderr"), "log");
    assert_eq!(int(record, "bytes"), 1024, "{record:?}");
    assert!(!flag(record, "complete"), "{record:?}");
    assert!(
        optional_text(record, "error").is_some(),
        "a short log must say why: {record:?}"
    );
    assert_eq!(
        std::fs::metadata(&log).expect("the log exists").len(),
        1024,
        "the file holds exactly what was written"
    );
    assert!(!flag(&result, "success"), "{result:?}");
}

// ── bounds and the general record ───────────────────────────────────────

#[test]
fn a_timeout_with_unread_stdin_still_returns_bounded() {
    let mut pairs = child(
        "sleep-ignore-stdin",
        &[(SLEEP_MS, Some("120000")), (WRITE, Some("0"))],
    );
    pairs.push(("stdin", Value::Str("u".repeat(512 * 1024))));
    pairs.push(("timeout_ms", Value::Int(300)));

    let started = Instant::now();
    let result = record(Some(options(pairs)));
    let elapsed = started.elapsed();

    let process = record_of(&result, "process");
    assert!(flag(process, "timed_out"), "{process:?}");
    assert!(flag(process, "cleanup_complete"), "{process:?}");
    assert!(
        flag(stdin_report(&result), "requested"),
        "the stdin record must describe the stdin that was sent"
    );
    assert!(
        elapsed < Duration::from_secs(30),
        "the deadline must cover the writer too, took {elapsed:?}"
    );
}

#[test]
fn every_documented_field_is_present() {
    let result = record(Some(options(child("payload", &[(VALUE, Some("x"))]))));

    assert!(field(&result, "success").type_name() == "Bool");
    assert!(field(&result, "process").type_name() == "Record");
    assert!(field(&result, "stdin").type_name() == "Record");
    assert!(field(&result, "stdout").type_name() == "Record");
    assert!(field(&result, "stderr").type_name() == "Record");

    for key in ["capture", "total_bytes", "offset_bytes", "log"] {
        for stream in ["stdout", "stderr"] {
            assert!(
                record_of(&result, stream).contains_key(key),
                "{stream} is missing `{key}`"
            );
        }
    }
    for key in ["requested", "bytes", "written_bytes", "complete", "error"] {
        assert!(
            stdin_report(&result).contains_key(key),
            "stdin is missing `{key}`"
        );
    }
}

// ── the failures that leave something behind ───────────────────────────

#[test]
fn an_incomplete_stdin_clears_the_overall_verdict() {
    // The fixture never reads its stdin, so no pipe can accept all of this and
    // the writer reports a short write. The child still exits zero, so the
    // overall verdict must say the call was incomplete while `process.success`
    // answers for the child alone.
    let mut pairs = child("payload", &[(VALUE, Some("x"))]);
    pairs.push(("stdin", Value::Str("s".repeat(8 * 1024 * 1024))));

    let result = record(Some(options(pairs)));
    let process = record_of(&result, "process");
    assert!(flag(process, "success"), "{process:?}");
    assert_eq!(int(process, "exit_code"), 0, "{process:?}");
    assert!(!flag(process, "timed_out"), "{process:?}");
    assert!(!flag(process, "output_limit_exceeded"), "{process:?}");
    assert!(flag(process, "group_termination_requested"), "{process:?}");
    assert!(flag(process, "cleanup_complete"), "{process:?}");
    assert!(flag(record_of(&result, "stdout"), "complete"), "{result:?}");
    assert!(flag(record_of(&result, "stderr"), "complete"), "{result:?}");

    let stdin = stdin_report(&result);
    assert!(flag(stdin, "requested"), "{stdin:?}");
    assert_eq!(int(stdin, "bytes"), 8 * 1024 * 1024, "{stdin:?}");
    assert!(
        !flag(stdin, "complete"),
        "the child read none of it: {stdin:?}"
    );
    assert!(
        optional_text(stdin, "error").is_some(),
        "a short write must say so: {stdin:?}"
    );
    assert!(
        int(stdin, "written_bytes") < int(stdin, "bytes"),
        "{stdin:?}"
    );
    assert!(
        !flag(&result, "success"),
        "an incomplete stdin must not read as success: {result:?}"
    );
}

#[cfg(windows)]
#[test]
fn an_unresolvable_bare_name_in_a_custom_context_is_not_found() {
    // The bare name is this test binary's own file name, which the ambient
    // search of `CreateProcess` would find next to this executable. With a
    // requested directory and an empty child `PATH` it must not resolve at all,
    // so the marker fixture never runs.
    let dir = TempDir::new("bare-name");
    let marker = dir.join("started.txt");
    let binary = executable();
    let name = Path::new(&binary)
        .file_name()
        .expect("the test binary has a file name")
        .to_string_lossy()
        .into_owned();

    let mut environment = BTreeMap::new();
    environment.insert(MODE.to_string(), Value::Str("marker".into()));
    environment.insert(
        MARKER.to_string(),
        Value::Str(marker.to_string_lossy().into_owned()),
    );
    environment.insert("PATH".to_string(), Value::Null);

    let error = refusal_with(
        Value::Array(vec![Value::Str(name)]),
        Some(options(vec![
            ("cwd", Value::Str(dir.as_str())),
            ("env", Value::Record(environment)),
        ])),
    );
    assert!(error.contains("E_TOOL_MISSING"), "not coded: {error}");
    assert!(
        error.contains("not found in the requested working directory"),
        "the refusal must say what was searched: {error}"
    );
    assert!(
        !marker.exists(),
        "the fixture ran through the ambient search: {error}"
    );
}

#[test]
fn a_spawn_failure_names_the_log_it_left_behind() {
    // The logs are created before the child exists. When the program cannot be
    // started at all, the error must name the files it left, and those files
    // must be empty.
    let dir = TempDir::new("spawn-fail");
    let program = dir.join(if cfg!(windows) { "bad.exe" } else { "bad" });
    std::fs::write(&program, b"this is not an executable image\n")
        .expect("write the unusable program");
    let log = dir.join("out.log");

    let error = refusal_with(
        Value::Array(vec![Value::Str(program.to_string_lossy().into_owned())]),
        Some(options(vec![(
            "stdout_log",
            Value::Str(log.to_string_lossy().into_owned()),
        )])),
    );
    assert!(
        error.contains("E_TOOL_FAILED") || error.contains("E_TOOL_MISSING"),
        "not coded: {error}"
    );
    assert!(
        error.contains("log files were created empty before this failure"),
        "the failure must say what it left behind: {error}"
    );
    assert!(
        error.contains("out.log"),
        "the failure must name the log file: {error}"
    );
    assert!(log.exists(), "the log was created before the spawn failed");
    assert_eq!(
        std::fs::metadata(&log).expect("the log exists").len(),
        0,
        "a spawn failure leaves the log empty"
    );
}

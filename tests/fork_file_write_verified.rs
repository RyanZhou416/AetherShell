//! `file_write_verified` end to end: the exact bytes each codec produces, the
//! option contract, the preconditions that must refuse before anything is
//! written, and the result record the caller branches on.
//!
//! Every assertion about content reads the file back with `std::fs` rather than
//! trusting the builtin's own `observed_*` fields, and digests are recomputed
//! here with `sha2`. A test that asked the builtin to confirm itself would pass
//! against an implementation that reported what it intended instead of what it
//! wrote, which is the defect this builtin exists to remove.
//!
//! **This file never writes the process environment.** Mode, workspace and
//! policy are properties of a *process*, and this binary runs its tests in
//! parallel threads over one environment, so setting them here would decide the
//! outcome of unrelated tests. They are asserted instead of repaired
//! ([`assert_human_mode`]), and the one case that genuinely needs agent mode
//! runs in a child of this same build with those variables set on that child's
//! own command.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use aethershell::safety::{current_mode, Mode, SafetyError};
use aethershell::value::Value;
use sha2::{Digest, Sha256};

/// `中文测试\n` in GBK: not valid UTF-8, so a decoder or encoder that guessed
/// would be caught here.
const GBK_BYTES: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4, b'\n'];
const GBK_TEXT: &str = "中文测试\n";
/// A little-endian mark, `A`, `é`.
const UTF16LE_BOM_BYTES: &[u8] = &[0xFF, 0xFE, 0x41, 0x00, 0xE9, 0x00];
/// The same text big-endian.
const UTF16BE_BOM_BYTES: &[u8] = &[0xFE, 0xFF, 0x00, 0x41, 0x00, 0xE9];
const UTF16_TEXT: &str = "Aé";
/// The fields the result record carries, and nothing else.
const RESULT_FIELDS: [&str; 12] = [
    "success",
    "path",
    "written",
    "verified",
    "encoding",
    "bom",
    "expected_bytes",
    "observed_bytes",
    "expected_sha256",
    "observed_sha256",
    "precondition_sha256",
    "verification",
];

// ── a test's own directory ──────────────────────────────────────────────

/// The human profile every in-process test here depends on.
///
/// A prerequisite, not a repair: nothing in this file is allowed to clear the
/// mode for the next test, so an environment that says otherwise is reported
/// rather than overridden.
fn assert_human_mode() {
    assert_eq!(
        current_mode(),
        Mode::Human,
        "these tests describe human mode; run them without AETHER_MODE or AETHER_AGENT set"
    );
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        assert_human_mode();
        Self {
            dir: tempfile::tempdir().expect("scratch directory"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The directory's own entries, so a refusal can be shown to leave no
    /// temporary file behind.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path())
            .expect("readable scratch directory")
            .map(|entry| {
                entry
                    .expect("readable entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort_unstable();
        names
    }
}

// ── calling the builtin ─────────────────────────────────────────────────

fn run(path: &Path, content: &str, options: Option<&[(&str, Value)]>) -> anyhow::Result<Value> {
    let mut args = vec![
        Value::Str(path.to_string_lossy().into_owned()),
        Value::Str(content.to_string()),
    ];
    if let Some(pairs) = options {
        let mut record = BTreeMap::new();
        for (key, value) in pairs {
            record.insert((*key).to_string(), value.clone());
        }
        args.push(Value::Record(record));
    }
    aethershell::file_write_verified::run(args, None)
}

fn run_args(args: Vec<Value>) -> anyhow::Result<Value> {
    aethershell::file_write_verified::run(args, None)
}

/// The `E_*` code a refusal carries, or `<uncoded>` when the boundary let a bare
/// error through — which is itself a defect this assertion surfaces.
fn code(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<SafetyError>()
        .map(|safety| safety.code.as_str().to_string())
        .unwrap_or_else(|| "<uncoded>".to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn option<'a>(key: &'a str, value: &'a str) -> (&'a str, Value) {
    (key, Value::Str(value.to_string()))
}

// ── reading the result record ───────────────────────────────────────────

fn record(value: &Value) -> &BTreeMap<String, Value> {
    match value {
        Value::Record(fields) => fields,
        other => panic!("expected a Record result, got {}", other.type_name()),
    }
}

fn boolean(fields: &BTreeMap<String, Value>, key: &str) -> bool {
    match fields.get(key) {
        Some(Value::Bool(flag)) => *flag,
        other => panic!("`{key}` should be a Bool, got {other:?}"),
    }
}

fn text(fields: &BTreeMap<String, Value>, key: &str) -> String {
    match fields.get(key) {
        Some(Value::Str(text)) => text.clone(),
        other => panic!("`{key}` should be a String, got {other:?}"),
    }
}

fn int(fields: &BTreeMap<String, Value>, key: &str) -> i64 {
    match fields.get(key) {
        Some(Value::Int(number)) => *number,
        other => panic!("`{key}` should be an Int, got {other:?}"),
    }
}

fn seed(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("seed the target");
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("the target must be readable")
}

// ── the encodings ───────────────────────────────────────────────────────

#[test]
fn utf8_is_the_exact_bytes_and_the_digest_of_them() {
    let fixture = Fixture::new();
    let target = fixture.path("utf8.txt");
    let content = "héllo — ✓\n";

    let value = run(&target, content, None).expect("a plain write must succeed");
    let fields = record(&value);

    assert_eq!(
        read(&target),
        content.as_bytes(),
        "the file must hold exactly the encoded String"
    );
    assert!(boolean(fields, "success"));
    assert!(boolean(fields, "written"));
    assert!(boolean(fields, "verified"));
    assert_eq!(text(fields, "encoding"), "utf-8");
    assert_eq!(text(fields, "bom"), "none");
    assert_eq!(int(fields, "expected_bytes"), content.len() as i64);
    assert_eq!(int(fields, "observed_bytes"), content.len() as i64);
    assert_eq!(text(fields, "expected_sha256"), sha256_hex(&read(&target)));
    assert_eq!(text(fields, "observed_sha256"), sha256_hex(&read(&target)));
    assert_eq!(fields.get("precondition_sha256"), Some(&Value::Null));
    assert_eq!(text(fields, "path"), target.to_string_lossy());

    let verification = record(fields.get("verification").expect("the report"));
    assert_eq!(text(verification, "method"), "readback-bytes-sha256");
    assert!(boolean(verification, "matched"));
    assert_eq!(verification.get("error"), Some(&Value::Null));
    assert_eq!(fixture.entries(), vec!["utf8.txt".to_string()]);
}

#[test]
fn gbk_is_gbk_and_never_a_replacement_character() {
    let fixture = Fixture::new();
    let target = fixture.path("gbk.txt");

    let value = run(&target, GBK_TEXT, Some(&[option("encoding", "gbk")]))
        .expect("GBK is a supported codec");
    let fields = record(&value);

    assert_eq!(read(&target), GBK_BYTES);
    assert_eq!(text(fields, "encoding"), "gbk");
    assert_eq!(int(fields, "expected_bytes"), GBK_BYTES.len() as i64);
    assert!(boolean(fields, "verified"));
}

#[test]
fn a_character_gbk_cannot_map_is_refused_before_the_file_exists() {
    let fixture = Fixture::new();
    let target = fixture.path("unmappable.txt");

    let error = run(&target, "ok 😀\n", Some(&[option("encoding", "gbk")]))
        .expect_err("GBK cannot represent an emoji");

    assert_eq!(code(&error), "E_BAD_ARG");
    assert!(
        !target.exists(),
        "a codec failure must be settled before the path is touched"
    );
    assert!(fixture.entries().is_empty());
}

#[test]
fn utf16_boms_are_the_mark_then_the_code_units() {
    let fixture = Fixture::new();
    let little = fixture.path("le.txt");
    let big = fixture.path("be.txt");

    let value = run(
        &little,
        UTF16_TEXT,
        Some(&[option("encoding", "utf-16le"), option("bom", "add")]),
    )
    .expect("UTF-16LE with a mark");
    run(
        &big,
        UTF16_TEXT,
        Some(&[option("encoding", "utf-16be"), option("bom", "add")]),
    )
    .expect("UTF-16BE with a mark");

    assert_eq!(read(&little), UTF16LE_BOM_BYTES);
    assert_eq!(read(&big), UTF16BE_BOM_BYTES);
    let fields = record(&value);
    assert_eq!(
        int(fields, "expected_bytes"),
        UTF16LE_BOM_BYTES.len() as i64
    );
    assert_eq!(text(fields, "bom"), "add");
    assert!(boolean(fields, "verified"));
}

#[test]
fn the_envelope_is_enforced_before_anything_is_written() {
    let fixture = Fixture::new();
    let marked = fixture.path("marked.txt");
    let gbk_marked = fixture.path("gbk_marked.txt");
    let too_big = fixture.path("too_big.txt");

    // Six bytes of payload plus a two-byte mark is eight; a five-byte envelope
    // must refuse, which a bound that counted only the payload would not.
    let error = run(
        &marked,
        UTF16_TEXT,
        Some(&[
            option("encoding", "utf-16le"),
            option("bom", "add"),
            ("max_bytes", Value::Int(5)),
        ]),
    )
    .expect_err("the mark is part of the encoded document");
    assert_eq!(code(&error), "E_BUDGET_EXCEEDED");

    let error = run(
        &gbk_marked,
        GBK_TEXT,
        Some(&[option("encoding", "gbk"), option("bom", "add")]),
    )
    .expect_err("GBK has no byte-order mark");
    assert_eq!(code(&error), "E_BAD_ARG");

    let error = run(&too_big, "abcd", Some(&[("max_bytes", Value::Int(3))]))
        .expect_err("the document is over the caller's envelope");
    assert_eq!(code(&error), "E_BUDGET_EXCEEDED");

    assert!(fixture.entries().is_empty());
}

#[test]
fn a_content_bom_is_the_callers_text_and_is_not_stripped() {
    let fixture = Fixture::new();
    let target = fixture.path("doubled.txt");

    run(&target, "\u{feff}x", Some(&[option("bom", "add")]))
        .expect("a U+FEFF in the content is data");

    assert_eq!(
        read(&target),
        b"\xEF\xBB\xBF\xEF\xBB\xBFx",
        "the added mark and the caller's own U+FEFF are both written"
    );
}

// ── the option and argument contract ────────────────────────────────────

#[test]
fn the_argument_and_option_contract_is_settled_before_any_file_work() {
    let fixture = Fixture::new();
    let target = fixture.path("refused.txt");

    let error = run(&target, "payload\n", Some(&[option("mode", "atomic")]))
        .expect_err("an unknown option key is a refusal");
    assert_eq!(code(&error), "E_BAD_ARG");

    // Raw option values, one per key, so the type enforcement is exercised for
    // every key rather than only the one that happened to be written first.
    for (key, value) in [
        ("max_bytes", Value::Str("1024".to_string())),
        ("create_only", Value::Int(1)),
        ("encoding", Value::Int(1)),
        ("bom", Value::Bool(true)),
    ] {
        let error = run(&target, "payload\n", Some(&[(key, value)]))
            .expect_err("a wrong-typed option is a refusal");
        assert_eq!(code(&error), "E_BAD_ARG", "the option {key:?}");
    }

    // Only the four documented spellings are encodings, and only the two
    // documented spellings are BOM modes. An uppercase form or the bare `utf8`
    // is not a second alias for `utf-8`: guessing one would write a file in a
    // codec, or with a mark, the caller never named.
    for label in ["UTF-8", "utf8", "UTF8", "utf-16", "latin1", "Utf-8"] {
        let error = run(&target, "payload\n", Some(&[option("encoding", label)]))
            .expect_err("an unrecognized encoding label is refused");
        assert_eq!(code(&error), "E_BAD_ARG", "the encoding label {label:?}");
    }
    for label in ["NONE", "None", "Add", "ADD", "true", "yes", ""] {
        let error = run(&target, "payload\n", Some(&[option("bom", label)]))
            .expect_err("an unrecognized bom label is refused");
        assert_eq!(code(&error), "E_BAD_ARG", "the bom label {label:?}");
    }

    let error = run(
        &target,
        "payload\n",
        Some(&[option("expected_sha256", "not-a-digest")]),
    )
    .expect_err("a short digest is not a digest");
    assert_eq!(code(&error), "E_BAD_ARG");

    let error = run_args(vec![
        Value::Str(target.to_string_lossy().into_owned()),
        Value::Str("payload\n".to_string()),
        Value::Record(BTreeMap::new()),
        Value::Str("and another".to_string()),
    ])
    .expect_err("the call takes at most three arguments");
    assert_eq!(code(&error), "E_BAD_ARG");

    // The parent does not exist either. The content's type is the defect the
    // caller must hear about: a call that never had a String to write must not
    // be diagnosed as a missing directory.
    let absent = fixture.path("absent").join("payload.txt");
    let error = run_args(vec![
        Value::Str(absent.to_string_lossy().into_owned()),
        Value::Int(7),
    ])
    .expect_err("content is a String");
    assert_eq!(code(&error), "E_BAD_ARG");

    assert!(
        fixture.entries().is_empty(),
        "a refused call must not create the target or a temporary file"
    );
}

#[test]
fn piped_input_is_refused_rather_than_ignored() {
    let fixture = Fixture::new();
    let target = fixture.path("piped.txt");

    let error = aethershell::file_write_verified::run(
        vec![
            Value::Str(target.to_string_lossy().into_owned()),
            Value::Str("explicit\n".to_string()),
        ],
        Some(Value::Str("piped\n".to_string())),
    )
    .expect_err("a piped document is never written silently");

    assert_eq!(code(&error), "E_BAD_ARG");
    assert!(!target.exists());
}

// ── preconditions that must refuse before anything is written ───────────

#[test]
fn a_missing_parent_directory_is_not_found_and_writes_nothing() {
    let fixture = Fixture::new();
    let target = fixture.path("absent").join("payload.txt");

    let error = run(&target, "payload\n", None).expect_err("the directory does not exist");

    assert_eq!(code(&error), "E_NOT_FOUND");
    assert!(fixture.entries().is_empty());
}

#[test]
fn a_failed_expected_sha256_precondition_writes_nothing() {
    let fixture = Fixture::new();
    let absent = fixture.path("absent.txt");
    let present = fixture.path("changed.txt");
    let original = b"original\n";
    seed(&present, original);

    let digest = sha256_hex(b"whatever the caller believed");
    let error = run(
        &absent,
        "payload\n",
        Some(&[option("expected_sha256", &digest)]),
    )
    .expect_err("a precondition on content cannot hold for a file that is not there");
    assert_eq!(code(&error), "E_NOT_FOUND");
    assert!(!absent.exists(), "a missing target must not be created");

    let digest = sha256_hex(b"a different file");
    let error = run(
        &present,
        "replacement\n",
        Some(&[option("expected_sha256", &digest)]),
    )
    .expect_err("the target is not the file the caller expected");
    assert_eq!(code(&error), "E_BAD_ARG");
    assert_eq!(read(&present), original, "the target must be untouched");

    // A target longer than the envelope is refused rather than hashed from a
    // prefix: the precondition would otherwise be checked against a digest that
    // describes bytes the file does not have.
    let oversized = fixture.path("oversized.txt");
    let long = vec![b'x'; 4096];
    seed(&oversized, &long);
    let error = run(
        &oversized,
        "replacement\n",
        Some(&[
            option("expected_sha256", &sha256_hex(&long)),
            ("max_bytes", Value::Int(64)),
        ]),
    )
    .expect_err("the target is past the envelope, so it cannot be hashed");
    assert_eq!(code(&error), "E_BUDGET_EXCEEDED");
    assert_eq!(read(&oversized), long, "the target must be untouched");

    assert_eq!(
        fixture.entries(),
        vec!["changed.txt".to_string(), "oversized.txt".to_string()]
    );
}

#[test]
fn expected_sha256_that_matches_is_echoed_lowercased() {
    let fixture = Fixture::new();
    let target = fixture.path("precondition.txt");
    let original = b"original\n";
    seed(&target, original);
    let digest = sha256_hex(original);

    let value = run(
        &target,
        "replacement\n",
        Some(&[option("expected_sha256", &digest.to_ascii_uppercase())]),
    )
    .expect("the precondition holds");
    let fields = record(&value);

    assert_eq!(
        text(fields, "precondition_sha256"),
        digest,
        "the option is echoed in the form the comparison used"
    );
    assert_eq!(
        text(fields, "expected_sha256"),
        sha256_hex(b"replacement\n"),
        "the result digest describes the bytes this call wrote"
    );
    assert_eq!(read(&target), b"replacement\n");
}

#[test]
fn create_only_writes_a_new_file_and_refuses_an_existing_one() {
    let fixture = Fixture::new();
    let fresh = fixture.path("fresh.txt");
    let occupied = fixture.path("occupied.txt");
    let original = b"do not replace me\n";
    seed(&occupied, original);

    let value = run(
        &fresh,
        "first\n",
        Some(&[("create_only", Value::Bool(true))]),
    )
    .expect("the target does not exist");
    assert!(boolean(record(&value), "verified"));
    assert_eq!(read(&fresh), b"first\n");

    let error = run(
        &occupied,
        "replacement\n",
        Some(&[("create_only", Value::Bool(true))]),
    )
    .expect_err("the target already exists");

    assert_eq!(code(&error), "E_BAD_ARG");
    assert_eq!(read(&occupied), original);
    assert_eq!(
        fixture.entries(),
        vec!["fresh.txt".to_string(), "occupied.txt".to_string()]
    );
}

#[test]
fn create_only_and_expected_sha256_together_are_refused() {
    let fixture = Fixture::new();
    let target = fixture.path("contradiction.txt");

    let error = run(
        &target,
        "payload\n",
        Some(&[
            ("create_only", Value::Bool(true)),
            option("expected_sha256", &sha256_hex(b"anything")),
        ]),
    )
    .expect_err("one requires the target to be absent and the other requires it to exist");

    assert_eq!(code(&error), "E_BAD_ARG");
    assert!(fixture.entries().is_empty());
}

#[test]
fn an_existing_directory_is_refused_rather_than_replaced() {
    let fixture = Fixture::new();
    let target = fixture.path("a_directory");
    std::fs::create_dir(&target).expect("create the directory");

    let error = run(&target, "payload\n", None).expect_err("a directory is not a file");

    assert_eq!(code(&error), "E_BAD_ARG");
    assert!(target.is_dir(), "the directory must still be there");
}

// ── the record ──────────────────────────────────────────────────────────

#[test]
fn the_record_is_the_contract_and_the_publish_leaves_no_temporary_file() {
    let fixture = Fixture::new();
    let target = fixture.path("shape.txt");

    let value = run(&target, "payload\n", None).expect("a plain write");
    let fields = record(&value);

    let mut present: Vec<&str> = fields.keys().map(String::as_str).collect();
    present.sort_unstable();
    let mut expected = RESULT_FIELDS.to_vec();
    expected.sort_unstable();
    assert_eq!(
        present, expected,
        "the record's fields are the contract an agent branches on"
    );

    let verification = record(fields.get("verification").expect("the report"));
    let mut present: Vec<&str> = verification.keys().map(String::as_str).collect();
    present.sort_unstable();
    assert_eq!(present, vec!["error", "matched", "method"]);

    assert_eq!(
        fixture.entries(),
        vec!["shape.txt".to_string()],
        "the temporary file is consumed by the publish"
    );
}

// ── the target the caller named ─────────────────────────────────────────

#[cfg(unix)]
#[test]
fn a_read_only_target_is_refused_with_a_policy_code() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let target = fixture.path("locked.txt");
    let original = b"original\n";
    seed(&target, original);
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444))
        .expect("make it read-only");

    let error = run(&target, "replacement\n", None).expect_err("the target is read-only");

    assert_eq!(code(&error), "E_POLICY_DENY");
    assert_eq!(read(&target), original);

    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
        .expect("restore the mode for cleanup");
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_target_is_refused_rather_than_followed() {
    let fixture = Fixture::new();
    let real = fixture.path("real.txt");
    let link = fixture.path("link.txt");
    let original = b"original\n";
    seed(&real, original);
    std::os::unix::fs::symlink(&real, &link).expect("create the link");

    let error = run(&link, "replacement\n", None).expect_err("a link is not a file");

    assert_eq!(code(&error), "E_POLICY_DENY");
    assert_eq!(
        read(&real),
        original,
        "the write must not have reached whatever the link pointed at"
    );
}

// ── the workspace jail, in a child of this build ────────────────────────

/// A child that is killed if the test leaves before the child is reaped.
///
/// `ae agent execute` is a one-shot, input-driven call: it reads one JSON
/// request from stdin, answers on stdout and exits. Nothing in it waits on a
/// person, so this guard is for the unwind path — a failed assertion must not
/// leave a process of this build running.
struct OwnedChild(Option<Child>);

impl OwnedChild {
    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("the child is still owned")
    }

    /// Take the child out, leaving the guard empty so its `Drop` is a no-op.
    fn into_child(mut self) -> Child {
        self.0.take().expect("the child is still owned")
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Run one `call` request through this build's own binary and return its reply.
///
/// The jail needs agent mode and a workspace root, and those are properties of
/// the process being tested. Setting them here would decide the outcome of every
/// other test in this binary, so they are set on the child's own command and the
/// parent environment is never written. The child's working directory is the
/// workspace, which is what a relative path in a request would resolve against.
fn shell_call(workspace: &Path, target: &Path, content: &str) -> serde_json::Value {
    let request = serde_json::json!({
        "action": "call",
        "builtin": "file_write_verified",
        "args": [target.to_string_lossy(), content],
    })
    .to_string();

    let mut command = Command::new(env!("CARGO_BIN_EXE_ae"));
    command
        .arg("agent")
        .arg("execute")
        .current_dir(workspace)
        .env("AETHER_MODE", "agent")
        .env("AETHER_WORKSPACE", workspace)
        .env("AETHER_POLICY", "strict")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Inherited operator settings that would change this call's answer rather
    // than exercise it: an approval token, an audit destination elsewhere, and
    // the resource envelopes. Removed from the child only.
    for name in [
        "AETHER_APPROVE",
        "AETHER_AUDIT_LOG",
        "AETHER_MAX_OPS",
        "AETHER_MAX_FILES",
        "AETHER_MAX_PROCS",
        "AETHER_MAX_NET",
        "AETHER_TIMEOUT_MS",
    ] {
        command.env_remove(name);
    }

    let spawned = command.spawn().expect("this build's own binary must start");
    let mut child = OwnedChild(Some(spawned));
    {
        let mut stdin = child.child_mut().stdin.take().expect("piped stdin");
        stdin
            .write_all(request.as_bytes())
            .expect("the request must reach the shell");
        // Dropping the handle here is what tells the child the request ended;
        // the child writes a bounded reply, so reading it afterwards cannot
        // fill a pipe and block it.
    }
    let output = child
        .into_child()
        .wait_with_output()
        .expect("the shell must finish");

    assert!(
        output.status.success(),
        "`ae agent execute` exited with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("the reply is UTF-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("the reply must be JSON: {e}\nstdout: {stdout}"))
}

#[test]
fn a_target_outside_the_workspace_is_refused() {
    // Proves the parent environment is untouched: the child below is the only
    // process that has ever been in agent mode.
    assert_human_mode();
    let workspace = tempfile::tempdir().expect("workspace fixture");
    let outside = tempfile::tempdir().expect("outside fixture");
    let inside = workspace.path().join("inside_payload.txt");
    let refused = outside.path().join("outside_payload.txt");

    let reply = shell_call(workspace.path(), &refused, "payload\n");
    assert_eq!(
        reply["success"],
        serde_json::Value::Bool(false),
        "a write outside the workspace must be refused: {reply}"
    );
    // Two layers can refuse this and either answer is correct: the path
    // validator's containment check (`E_OUTSIDE_WORKSPACE`) runs first, and the
    // guard's workspace jail catches whatever reached it.
    let code = reply["metadata"]["code"].as_str().unwrap_or_default();
    assert!(
        code == "E_OUTSIDE_WORKSPACE" || code == "E_POLICY_DENY",
        "expected a containment refusal, got {code:?}: {reply}"
    );
    assert!(
        !refused.exists(),
        "nothing may be created outside the workspace"
    );

    // The same call inside the workspace must still work: the jail is
    // containment, not a ban.
    let reply = shell_call(workspace.path(), &inside, "payload\n");
    assert_eq!(
        reply["success"],
        serde_json::Value::Bool(true),
        "a write inside the workspace must be allowed: {reply}"
    );
    assert_eq!(reply["result"]["verified"], serde_json::Value::Bool(true));
    assert_eq!(read(&inside), b"payload\n");
}

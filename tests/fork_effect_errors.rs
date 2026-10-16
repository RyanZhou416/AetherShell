//! The refusal contract of the guarded builtins that reach the filesystem and
//! the process environment.
//!
//! `rm`/`rmdir` report every failure except an absent target, and they refuse a
//! wrong-typed flag before the guard; the environment builtins refuse a name or
//! value the platform call cannot honour instead of panicking the shell.
//!
//! Every case runs in a child of this same build with its own environment and
//! working directory. `AETHER_MODE` and the process environment are global to
//! this binary and its tests share them, so a case here never sets a variable on
//! the test process.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::ffi::OsStr;

use aethershell::safety::ErrorCode;

/// A child still running after this is hung, not slow: every source here is one
/// builtin call.
const DEADLINE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(5);
/// Every source under test answers in one line; this bounds the drain.
const OUTPUT_CAP: u64 = 64 * 1024;

/// A scratch directory removed when the case ends, whatever the assertions did.
struct Case {
    dir: tempfile::TempDir,
}

impl Case {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("scratch directory"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Create `name` as a directory (with parents) inside the case.
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.path(name);
        std::fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    fn run(&self, source: &str) -> Run {
        run_in(self.dir.path(), source)
    }
}

/// What one child printed.
struct Run {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn render(&self) -> String {
        format!(
            "exit={:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.status, self.stdout, self.stderr
        )
    }

    fn succeeded(&self) {
        assert_eq!(self.status, Some(0), "expected success: {}", self.render());
        assert!(
            !self.stderr.contains("panicked"),
            "the shell panicked: {}",
            self.render()
        );
    }

    /// The `E_*` code the CLI printed on stderr.
    fn printed_code(&self) -> &str {
        let open = self
            .stderr
            .find("error[")
            .unwrap_or_else(|| panic!("no coded error was printed: {}", self.render()))
            + "error[".len();
        let close = self.stderr[open..]
            .find(']')
            .unwrap_or_else(|| panic!("the printed code is unterminated: {}", self.render()));
        &self.stderr[open..open + close]
    }

    /// Assert the failure is one of `codes`, that the exit status agrees with
    /// it, and that the shell refused rather than died.
    fn refused_with(&self, codes: &[ErrorCode]) {
        assert!(
            !self.stderr.contains("panicked"),
            "the shell panicked instead of refusing: {}",
            self.render()
        );
        let printed = self.printed_code();
        let expected: Vec<&str> = codes.iter().map(|code| code.as_str()).collect();
        assert!(
            expected.contains(&printed),
            "expected one of {:?}, got {}: {}",
            expected,
            printed,
            self.render()
        );
        let statuses: Vec<i32> = codes.iter().map(|code| code.exit_code()).collect();
        assert!(
            statuses.contains(&self.status.unwrap_or(-1)),
            "exit status does not match {}: {}",
            printed,
            self.render()
        );
    }

    fn refused(&self, code: ErrorCode) {
        self.refused_with(&[code]);
    }
}

/// Kills and reaps the child on every path out of a case, including a panic, so
/// a failed assertion never leaves an `ae` process behind.
struct OwnedChild(Child);

impl OwnedChild {
    fn wait_within(&mut self, source: &str) -> ExitStatus {
        let limit = Instant::now() + DEADLINE;
        loop {
            match self.0.try_wait().expect("poll the ae child") {
                Some(status) => return status,
                None if Instant::now() >= limit => {
                    panic!("ae did not exit within {DEADLINE:?}; source: {source}")
                }
                None => std::thread::sleep(POLL),
            }
        }
    }

    fn drain(&mut self) -> (String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        if let Some(pipe) = self.0.stdout.take() {
            let mut pipe = pipe.take(OUTPUT_CAP);
            let _ = pipe.read_to_end(&mut out);
        }
        if let Some(pipe) = self.0.stderr.take() {
            let mut pipe = pipe.take(OUTPUT_CAP);
            let _ = pipe.read_to_end(&mut err);
        }
        (
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The `ae` child for one case, with the posture these cases depend on.
fn ae_child(dir: &Path, source: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ae"));
    command
        .arg("--deterministic")
        .arg("-c")
        .arg(source)
        .current_dir(dir)
        // The child's posture is declared rather than inherited: the mode and
        // policy variables are read from the environment, and an operator's or a
        // sibling test's setting would otherwise decide these cases.
        .env("AETHER_MODE", "human")
        .env_remove("AETHER_AGENT")
        .env_remove("AETHER_WORKSPACE")
        .env_remove("AETHER_POLICY")
        .env_remove("AETHER_APPROVE")
        .env_remove("AETHER_APPROVE_ALL")
        // Any home-relative state stays inside the owned fixture.
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Wait, drain and report one child. The wait is bounded, so a source that hung
/// fails here instead of holding the test binary open.
fn collect(child: Child, source: &str) -> Run {
    let mut owned = OwnedChild(child);
    let status = owned.wait_within(source);
    let (stdout, stderr) = owned.drain();
    Run {
        status: status.code(),
        stdout,
        stderr,
    }
}

fn run_in(dir: &Path, source: &str) -> Run {
    collect(
        ae_child(dir, source).spawn().expect("spawn the ae binary"),
        source,
    )
}

/// One case, with an extra variable set on the child alone.
#[cfg(unix)]
fn run_in_with(dir: &Path, source: &str, extra: (&OsStr, &OsStr)) -> Run {
    let mut command = ae_child(dir, source);
    command.env(extra.0, extra.1);
    collect(command.spawn().expect("spawn the ae binary"), source)
}

/// A source expected to be refused with `code`, run as its own child: an invalid
/// name must be a refusal, not a process that dies mid-session.
fn assert_refused(source: &str, code: ErrorCode) {
    Case::new().run(source).refused(code);
}

// ── rm ──────────────────────────────────────────────────────────────────

#[test]
fn rm_force_removes_an_existing_file() {
    let case = Case::new();
    let file = case.path("owned.txt");
    std::fs::write(&file, b"x").expect("fixture file");

    let run = case.run(r#"rm("owned.txt", true)"#);

    run.succeeded();
    assert!(run.stdout.contains("removed"), "{}", run.render());
    assert!(!file.exists(), "the file survived: {}", run.render());
}

#[test]
fn rm_force_tolerates_only_an_absent_target() {
    let case = Case::new();
    let run = case.run(r#"rm("absent.txt", true)"#);

    run.succeeded();
    assert!(run.stdout.contains("rm (forced):"), "{}", run.render());
    assert!(
        !run.stdout.contains("removed"),
        "an absent target must not read as a removal: {}",
        run.render()
    );
    assert!(!case.path("absent.txt").exists(), "{}", run.render());
}

#[test]
fn rm_force_on_a_directory_is_refused_and_leaves_it_intact() {
    let case = Case::new();
    let folder = case.folder("owned-folder");
    let inner = case.path("owned-folder/inner.txt");
    std::fs::write(&inner, b"x").expect("fixture file");

    let run = case.run(r#"rm("owned-folder", true)"#);

    // A directory is neither absence nor a successful removal. Which of the two
    // coded failures it is depends on the platform: Windows refuses with access
    // denied, POSIX with "is a directory".
    run.refused_with(&[ErrorCode::PolicyDeny, ErrorCode::Io]);
    assert!(
        !run.stdout.contains("removed"),
        "a refused delete must not read as a removal: {}",
        run.render()
    );
    assert!(folder.is_dir(), "the directory went away: {}", run.render());
    assert!(
        inner.is_file(),
        "rm must not become recursive: {}",
        run.render()
    );
}

#[test]
fn rm_without_force_on_a_directory_is_refused_too() {
    let case = Case::new();
    let folder = case.folder("owned-folder");

    let run = case.run(r#"rm("owned-folder")"#);

    run.refused_with(&[ErrorCode::PolicyDeny, ErrorCode::Io]);
    assert!(folder.is_dir(), "{}", run.render());
}

#[test]
fn rm_refuses_a_non_bool_force_before_the_guard_and_the_filesystem() {
    let case = Case::new();
    let folder = case.folder("owned-folder");

    let run = case.run(r#"rm("owned-folder", "yes")"#);

    run.refused(ErrorCode::BadArg);
    assert!(
        folder.is_dir(),
        "a refused call must not delete: {}",
        run.render()
    );
}

// ── rmdir ───────────────────────────────────────────────────────────────

#[test]
fn rmdir_refuses_a_non_bool_recursive_flag() {
    let case = Case::new();
    let folder = case.folder("owned-folder");

    let run = case.run(r#"rmdir("owned-folder", 1)"#);

    run.refused(ErrorCode::BadArg);
    assert!(folder.is_dir(), "{}", run.render());
}

#[test]
fn rmdir_reports_an_absent_directory_as_not_found() {
    Case::new()
        .run(r#"rmdir("absent-folder")"#)
        .refused(ErrorCode::NotFound);
}

#[test]
fn rmdir_still_removes_directories() {
    let case = Case::new();
    let empty = case.folder("empty");
    case.run(r#"rmdir("empty")"#).succeeded();
    assert!(!empty.exists(), "the empty directory survived");

    case.folder("tree/inner");
    std::fs::write(case.path("tree/inner/file.txt"), b"x").expect("fixture file");
    case.run(r#"rmdir("tree", true)"#).succeeded();
    assert!(!case.path("tree").exists(), "the tree survived");
}

// ── environment names and values ────────────────────────────────────────

/// Names the platform call cannot take: empty, carrying `=`, and carrying a NUL
/// that `json_parse` decodes inside the child, so no NUL reaches this process's
/// argv.
const INVALID_NUL_NAME: &str = r#"json_parse("\"bad\\u0000key\"")"#;

#[test]
fn env_set_refuses_names_the_platform_call_cannot_take() {
    for source in [
        r#"env_set("", "value")"#.to_string(),
        r#"env_set("A=B", "value")"#.to_string(),
        format!(r#"env_set({INVALID_NUL_NAME}, "value")"#),
    ] {
        assert_refused(&source, ErrorCode::BadArg);
    }
}

#[test]
fn env_unset_refuses_names_the_platform_call_cannot_take() {
    for source in [
        r#"env_unset("")"#.to_string(),
        r#"env_unset("A=B")"#.to_string(),
        format!(r#"env_unset({INVALID_NUL_NAME})"#),
    ] {
        assert_refused(&source, ErrorCode::BadArg);
    }
}

#[test]
fn set_env_refuses_names_the_platform_call_cannot_take() {
    for source in [
        r#"set_env("", "value")"#.to_string(),
        r#"set_env("A=B", "value")"#.to_string(),
        format!(r#"set_env({INVALID_NUL_NAME}, "value")"#),
    ] {
        assert_refused(&source, ErrorCode::BadArg);
    }
}

#[test]
fn the_readers_refuse_the_names_the_writers_refuse() {
    for source in [
        r#"env("")"#,
        r#"env("A=B")"#,
        r#"env_var("")"#,
        r#"env_var("A=B")"#,
    ] {
        assert_refused(source, ErrorCode::BadArg);
    }
}

#[test]
fn a_nul_in_the_value_is_refused_and_the_value_is_not_echoed() {
    let run = Case::new()
        .run(r#"env_set("AE_FORK_OWNED_ENV", json_parse("\"TOPSECRET-CANARY\\u0000\""))"#);

    run.refused(ErrorCode::BadArg);
    assert!(
        !run.stderr.contains("TOPSECRET-CANARY"),
        "a refused value reached the diagnostic: {}",
        run.render()
    );
}

#[test]
fn a_valid_value_round_trips_byte_exactly() {
    // The value carries the two things a shell tends to rewrite: an
    // interpolation sigil (`${...}` is Ae syntax, so the literal has to come from
    // a raw literal) and a real CRLF, decoded by `json_parse`.
    let source = concat!(
        r##"env_set("AE_FORK_OWNED_ENV", json_parse(r#""${AE_FORK_LITERAL}-中文\r\nend""#)); "##,
        r#"env("AE_FORK_OWNED_ENV")"#,
    );

    let run = Case::new().run(source);

    run.succeeded();
    let rendered: serde_json::Value =
        serde_json::from_str(run.stdout.trim()).expect("deterministic output is JSON");
    assert_eq!(
        rendered.as_str(),
        Some("${AE_FORK_LITERAL}-中文\r\nend"),
        "{}",
        run.render()
    );
    assert!(
        std::env::var_os("AE_FORK_OWNED_ENV").is_none(),
        "the variable was set on the test process, not only on the child"
    );
}

#[test]
fn the_writer_return_shapes_are_unchanged() {
    let run = Case::new().run(r#"typeof(env_set("AE_FORK_OWNED_ENV", "one"))"#);
    run.succeeded();
    assert!(run.stdout.contains("Bool"), "{}", run.render());

    let run = Case::new().run(r#"typeof(set_env("AE_FORK_OWNED_ENV", "one"))"#);
    run.succeeded();
    assert!(run.stdout.contains("Null"), "{}", run.render());

    let run = Case::new().run(r#"typeof(env_unset("AE_FORK_OWNED_ENV"))"#);
    run.succeeded();
    assert!(run.stdout.contains("Bool"), "{}", run.render());
}

#[test]
fn an_absent_variable_still_takes_the_default_and_answers_null() {
    let name = format!("AE_FORK_MISSING_{}", std::process::id());

    let run = Case::new().run(&format!(r#"env("{name}", "fallback-ok")"#));
    run.succeeded();
    assert!(run.stdout.contains("fallback-ok"), "{}", run.render());

    let run = Case::new().run(&format!(r#"typeof(env_var("{name}"))"#));
    run.succeeded();
    assert!(run.stdout.contains("Null"), "{}", run.render());
}

/// A variable that is set to bytes which are not valid Unicode is not an absent
/// one, so the reader must refuse it rather than answer with `fallback`.
///
/// Unix-only: a Win32 environment block is UTF-16, so this process cannot hand a
/// child the unpaired surrogates that would make `VarError::NotUnicode` reachable
/// on Windows.
#[cfg(unix)]
#[test]
fn a_set_but_non_unicode_value_is_refused_rather_than_defaulted() {
    use std::os::unix::ffi::OsStrExt;

    const NAME: &str = "AE_FORK_NOT_UNICODE";
    let case = Case::new();
    let run = run_in_with(
        case.dir.path(),
        &format!(r#"env("{NAME}", "fallback")"#),
        (
            OsStr::from_bytes(NAME.as_bytes()),
            OsStr::from_bytes(&[0xff, 0xfe]),
        ),
    );

    run.refused(ErrorCode::BadArg);
    assert!(
        !run.stdout.contains("fallback"),
        "a set variable must not be reported as unset: {}",
        run.render()
    );
}

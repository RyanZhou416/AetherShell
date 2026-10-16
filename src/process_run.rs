//! `proc_run` — run one program from an explicit argv and report the process,
//! the decoding of its streams, and any log file it was asked to keep.
//!
//! The caller writes the program and every argument literally: no shell parses
//! them, so `|`, `*`, `$(...)` and quotes inside an argument are data. The child
//! becomes the leader of its own managed group — a POSIX process group, or a
//! Windows job object when this is a Windows build.
//!
//! # Arguments, environment and stdin
//!
//! `cwd` is resolved against the interpreter's own working directory,
//! canonicalized and checked to be an existing directory before anything runs,
//! and the child is started in it; the interpreter itself never changes
//! directory. `env` overrides or removes entries in the *child's* environment
//! only. The ambient environment is otherwise inherited and never scrubbed, and
//! no value is echoed in a result, an error or an audit entry. Names in the
//! `DSH_*`, `AETHER_*` and `AE_*` namespaces, and names that collide
//! case-insensitively on Windows, are refused before anything starts. That
//! namespace check is a convenience guard, not a security boundary: an `argv`
//! can still start a program that changes its own environment.
//!
//! `stdin` sends exactly those bytes and appends nothing; `stdin_base64` sends
//! arbitrary bytes decoded with strict base64; the two are mutually exclusive
//! and 16 MiB is the protocol maximum. A writer thread feeds the child's stdin
//! while both stream readers drain, so a child that writes before it reads
//! cannot deadlock the capture. `written_bytes` counts what the operating-system
//! pipe accepted — never evidence that the child consumed it.
//!
//! # Result
//!
//! ```text
//! { success: Bool
//! , process: { success: Bool, exit_code: Int|Null, timed_out: Bool
//!            , output_limit_exceeded: Bool, cleanup_complete: Bool
//!            , group_termination_requested: Bool }
//! , stdin: { requested: Bool, bytes: Int, written_bytes: Int
//!          , complete: Bool, error: String|Null }
//! , stdout: Stream
//! , stderr: Stream }
//!
//! Stream = { encoding: String, capture: String, bytes: Int, total_bytes: Int
//!          , offset_bytes: Int, base64: String, text: String|Null
//!          , decoded: Bool|Null, complete: Bool, error: String|Null
//!          , log: { path: String, bytes: Int, complete: Bool
//!                 , error: String|Null }|Null }
//! ```
//!
//! `bytes` counts the bytes the capture *retained*: the first `max_output_bytes`
//! under `capture: "prefix"`, or the last under `capture: "tail"`. `total_bytes`
//! is an exact count of the bytes read from that pipe, and `offset_bytes` is
//! where the retained bytes start in the stream — 0 for a prefix, and
//! `total_bytes - bytes` for a tail. `base64` always holds exactly the retained
//! bytes, so it stays meaningful when decoding fails. A stream whose reader is
//! still blocked on a pipe the process holds keeps what it has already read:
//! `complete` is false and an `error` says the capture is still open.
//!
//! A tail may begin in the middle of a multi-byte character or a UTF-16 code
//! unit; strict decoding then reports `decoded: false` with the raw bytes intact
//! rather than silently trimming text. A byte-order mark is **not** removed,
//! because the caller chose the encoding and the mark is part of the stream.
//!
//! A requested `stdout_log` or `stderr_log` receives the raw bytes exactly as
//! read, with no encoding, newline or interleaving applied, and is created
//! exclusively — an existing file is refused and never overwritten. `log.bytes`
//! counts the bytes actually written and `log.complete` is true only when the
//! whole stream read from that pipe reached the file. Reaching `max_log_bytes`,
//! or a write failure, keeps the reader draining so the child cannot block on a
//! full pipe, but asks the managed group to stop, because a log that cannot be
//! completed should not let the call run on.
//!
//! `process.success` means only "the leader exited normally with code 0".
//! `group_termination_requested` says the request to terminate the managed group
//! was accepted, or that the group was already gone: it is an acknowledgement,
//! not evidence that every process in the group has exited. `cleanup_complete`
//! is deliberately narrower and about this call's own resources — the leader was
//! reaped and every reader and writer thread started here was joined, so no pipe
//! or thread started here is left behind.
//!
//! The top-level `success` adds: no timeout, no discarded output, termination
//! requested, cleanup complete, every stream decoded, and every requested log
//! complete. A partial capture therefore reports `success: false` even when the
//! child exited zero: a prefix or tail preview is not the whole stream.
//! `process.success` is the field that answers for the child itself. A nonzero
//! exit, a truncated capture and a failed decode are reported, never raised —
//! the raw bytes and the exit status survive all three.
//!
//! # Decoding
//!
//! `encoding` names one codec exactly: no charset sniffing, no code page, no
//! replacement character. Bytes that are not valid in the named encoding give
//! `text: null`, `decoded: false` and an actionable `error`, with the bytes
//! intact in `base64`. Newlines are never rewritten.
//!
//! # Lifetime
//!
//! The child leads a native process group — a POSIX process group, or a Windows
//! job object — and this call *requests* termination of that group on every path
//! out: the normal one, an error, and a panic. The request is directed at the
//! whole group, so a descendant that stayed in it, including one still holding
//! the pipes, is in its scope; a leader that exits while such a descendant runs
//! is waited for only until `timeout_ms`. A descendant that leaves the group
//! (`setsid`, a Windows breakaway process) is not in that scope, and the call
//! then reports an incomplete capture rather than waiting without a bound.
//!
//! What comes back is the request and this call's own resources, not a guarantee
//! about the host. `group_termination_requested` is an acknowledgement from the
//! operating system and `cleanup_complete` says the leader was reaped and the
//! readers and writer were joined; neither proves that no descendant survives,
//! and no non-blocking check available here can prove it. Nothing in this call
//! runs if the interpreter itself is killed outright — the same reason in-process
//! cleanup cannot catch `SIGKILL` on POSIX — so descendants of an abruptly
//! terminated interpreter are outside this contract.
//!
//! This is process-group ownership, not a sandbox: the effect policy is the
//! control on what may be started. A requested log is an additional guarded
//! `WriteLocal` effect and never a way around a denied path.
//!
//! On Windows a `.cmd` or `.bat` program — including a bare name that `PATH`
//! resolves to one — is refused before anything starts, because running it means
//! `cmd.exe` interprets it, which is the implicit shell this builtin does not
//! have. Name the interpreter explicitly (`["cmd.exe", "/c", ...]`) to run one
//! on purpose.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;
use encoding_rs::Encoding;
#[cfg(windows)]
use process_wrap::std::JobObject;
#[cfg(unix)]
use process_wrap::std::ProcessGroup;
use process_wrap::std::{ChildWrapper, CommandWrap};

use crate::safety::{self, Effect, ErrorCode, GuardCtx, SafetyError};
use crate::value::Value;

const DEFAULT_TIMEOUT_MS: i64 = 120_000;
const MAX_TIMEOUT_MS: i64 = 600_000;
const DEFAULT_MAX_OUTPUT_BYTES: i64 = 1_048_576;
const MAX_OUTPUT_BYTES: i64 = 16_777_216;
const DEFAULT_MAX_LOG_BYTES: i64 = 67_108_864;
const MAX_LOG_BYTES: i64 = 1_073_741_824;
/// The most stdin this builtin will carry, in bytes. A protocol constant: an
/// adapter in front of it may impose a smaller one of its own.
const MAX_STDIN_BYTES: usize = 16_777_216;

/// How long a killed managed group gets to close its pipes and be reaped before
/// the call stops waiting for it.
const CLEANUP_GRACE: Duration = Duration::from_secs(5);
/// How long the wait loop sleeps before it re-reads the child and the captures.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// One read per syscall: 16 MiB of captured output is 256 reads.
const READ_CHUNK: usize = 65_536;

const ENCODING_EXPECTED: &str =
    "encoding to be one of \"utf-8\", \"gbk\", \"utf-16le\", \"utf-16be\" or \"bytes\"";
const CAPTURE_EXPECTED: &str = "capture to be \"prefix\" or \"tail\"";
const ON_LIMIT_EXPECTED: &str = "on_output_limit to be \"terminate\" or \"continue\"";
const OPTIONS_EXPECTED: &str = "an options Record whose keys are encoding, timeout_ms, \
     max_output_bytes, capture, on_output_limit, cwd, env, stdin, stdin_base64, stdout_log, \
     stderr_log and max_log_bytes";
const STREAMS: [&str; 2] = ["stdout", "stderr"];

/// Run `proc_run(argv, options?)` and answer its result record.
///
/// `input` is refused: the argv is the call, and silently accepting a piped
/// subject would make `x | proc_run(...)` mean something an agent cannot see.
/// Use `stdin` to feed the child.
pub fn run(args: Vec<Value>, input: Option<Value>) -> Result<Value> {
    let request = parse(args, input)?;

    // `proc_run` is classified `Effect::Exec` and listed in
    // `safety::SELF_GUARDED`, so `guard_dispatch` deliberately does not gate it;
    // this call is the gate. It runs after argument validation (a refused call
    // must not prompt) and before the process exists, and every route that
    // reaches this function — the dispatcher, `proc.run`, a direct call — passes
    // through it.
    safety::guard_exec("proc_run", audit_command(&request.argv))?;

    // A log is a second, `WriteLocal` effect of the same call. It is gated here,
    // before a file exists, so a denied path cannot create one and cannot start
    // the process either.
    guard_log_writes(&request)?;

    execute(request)
}

// ── arguments ───────────────────────────────────────────────────────────

/// The validated call: everything `execute` needs, resolved and bounded.
struct Request {
    argv: Vec<String>,
    /// The image to start. On Windows this is the path that was resolved and
    /// checked during validation, so the file inspected is the file started.
    #[cfg(windows)]
    image: std::ffi::OsString,
    encoding: Choice,
    timeout: Duration,
    max_output: usize,
    capture: Capture,
    on_limit: OnLimit,
    /// The child's working directory, canonicalized.
    cwd: Option<PathBuf>,
    /// The overrides applied to the child's environment; `None` removes a name.
    env: BTreeMap<String, Option<String>>,
    /// The bytes to write to the child's stdin, if any.
    stdin: Option<Vec<u8>>,
    logs: [Option<LogTarget>; 2],
    max_log: usize,
}

/// One validated `stdout_log`/`stderr_log` target.
struct LogTarget {
    /// The canonical path the file is created at.
    path: PathBuf,
    /// A returned spelling that preserves Win32-special path semantics.
    display: String,
    /// Exact textual target normalized for the native guard's lexical checks.
    guard_target: String,
}

/// One of the five encodings the caller may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Utf8,
    Gbk,
    Utf16Le,
    Utf16Be,
    /// No codec: the caller gets the bytes and nothing else.
    Bytes,
}

impl Choice {
    /// The exact spelling, or nothing: an unknown label is a refusal, never a
    /// guess at what the caller meant.
    fn parse(label: &str) -> Option<Self> {
        Some(match label {
            "utf-8" => Self::Utf8,
            "gbk" => Self::Gbk,
            "utf-16le" => Self::Utf16Le,
            "utf-16be" => Self::Utf16Be,
            "bytes" => Self::Bytes,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Gbk => "gbk",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
            Self::Bytes => "bytes",
        }
    }

    fn codec(self) -> Option<&'static Encoding> {
        match self {
            Self::Utf8 => Some(encoding_rs::UTF_8),
            Self::Gbk => Some(encoding_rs::GBK),
            Self::Utf16Le => Some(encoding_rs::UTF_16LE),
            Self::Utf16Be => Some(encoding_rs::UTF_16BE),
            Self::Bytes => None,
        }
    }
}

/// Which part of a stream the capture retains once it is longer than the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    Prefix,
    Tail,
}

impl Capture {
    fn parse(label: &str) -> Option<Self> {
        Some(match label {
            "prefix" => Self::Prefix,
            "tail" => Self::Tail,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Prefix => "prefix",
            Self::Tail => "tail",
        }
    }
}

/// What happens to the child when its output passes the caller's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnLimit {
    Terminate,
    Continue,
}

impl OnLimit {
    fn parse(label: &str) -> Option<Self> {
        Some(match label {
            "terminate" => Self::Terminate,
            "continue" => Self::Continue,
            _ => return None,
        })
    }
}

/// The option record as written, after shape validation and before resolution.
struct Options {
    encoding: Choice,
    timeout: Duration,
    max_output: usize,
    capture: Capture,
    on_limit: OnLimit,
    cwd: Option<String>,
    env: BTreeMap<String, Option<String>>,
    stdin: Option<Vec<u8>>,
    logs: [Option<String>; 2],
    max_log: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            encoding: Choice::Utf8,
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS as u64),
            max_output: DEFAULT_MAX_OUTPUT_BYTES as usize,
            capture: Capture::Prefix,
            on_limit: OnLimit::Terminate,
            cwd: None,
            env: BTreeMap::new(),
            stdin: None,
            logs: [None, None],
            max_log: DEFAULT_MAX_LOG_BYTES as usize,
        }
    }
}

fn parse(args: Vec<Value>, input: Option<Value>) -> Result<Request> {
    if let Some(piped) = input {
        return Err(safety::bad_arg(
            "proc_run",
            "no piped input: the argv is the first argument, and stdin is an option",
            piped.type_name(),
        ));
    }

    let mut args = args.into_iter();
    let argv = match args.next() {
        Some(value) => parse_argv(value)?,
        None => {
            return Err(safety::bad_arg(
                "proc_run",
                "argv as a non-empty Array of Strings",
                "nothing",
            ))
        }
    };
    let options = match args.next() {
        Some(Value::Record(options)) => parse_options(&options)?,
        Some(other) => {
            return Err(safety::bad_arg(
                "proc_run",
                OPTIONS_EXPECTED,
                other.type_name(),
            ))
        }
        None => Options::default(),
    };
    if let Some(extra) = args.next() {
        return Err(safety::bad_arg(
            "proc_run",
            "at most two arguments: argv and options",
            &format!("a third argument of type {}", extra.type_name()),
        ));
    }

    // Every path and environment decision is settled before the effect gates and
    // before anything exists: an invalid name, a missing directory or an
    // existing log must be refused without prompting, without writing and
    // without starting a program.
    let cwd = resolve_cwd(&options.cwd)?;
    let logs = resolve_logs(&options.logs, cwd.as_deref())?;

    // Resolved here, before the effect gate: a call that is going to be refused
    // — an unapproved program, or a Windows command script — must not prompt
    // first, and the path that is checked has to be the path that is started.
    // The ambient environment is never rewritten to do this.
    #[cfg(windows)]
    let image = resolve_image(&argv[0], cwd.as_deref(), &options.env)?;

    Ok(Request {
        argv,
        #[cfg(windows)]
        image,
        encoding: options.encoding,
        timeout: options.timeout,
        max_output: options.max_output,
        capture: options.capture,
        on_limit: options.on_limit,
        cwd,
        env: options.env,
        stdin: options.stdin,
        logs,
        max_log: options.max_log,
    })
}

fn parse_argv(value: Value) -> Result<Vec<String>> {
    let items = match value {
        Value::Array(items) => items,
        other => {
            return Err(safety::bad_arg(
                "proc_run",
                "argv as a non-empty Array of Strings",
                other.type_name(),
            ))
        }
    };
    if items.is_empty() {
        return Err(safety::bad_arg(
            "proc_run",
            "argv whose first element names the program",
            "an empty array",
        ));
    }

    let mut argv = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let text = match item {
            Value::Str(text) => text,
            other => {
                return Err(safety::bad_arg(
                    "proc_run",
                    &format!("argv[{index}] to be a String"),
                    other.type_name(),
                ))
            }
        };
        // A NUL cannot cross the process boundary at all; refusing it here says
        // so instead of letting the OS truncate the argument silently.
        if text.contains('\0') {
            return Err(safety::bad_arg(
                "proc_run",
                &format!("argv[{index}] to contain no NUL"),
                "a String containing U+0000",
            ));
        }
        argv.push(text);
    }
    if argv[0].is_empty() {
        return Err(safety::bad_arg(
            "proc_run",
            "argv[0] to name a program",
            "an empty String",
        ));
    }
    Ok(argv)
}

fn parse_options(options: &BTreeMap<String, Value>) -> Result<Options> {
    for key in options.keys() {
        if !matches!(
            key.as_str(),
            "encoding"
                | "timeout_ms"
                | "max_output_bytes"
                | "capture"
                | "on_output_limit"
                | "cwd"
                | "env"
                | "stdin"
                | "stdin_base64"
                | "stdout_log"
                | "stderr_log"
                | "max_log_bytes"
        ) {
            return Err(safety::bad_arg(
                "proc_run",
                OPTIONS_EXPECTED,
                &format!("the key {key:?}"),
            ));
        }
    }

    let encoding = match options.get("encoding") {
        None => Choice::Utf8,
        Some(Value::Str(label)) => Choice::parse(label).ok_or_else(|| {
            safety::bad_arg(
                "proc_run",
                ENCODING_EXPECTED,
                &format!("the label {label:?}"),
            )
        })?,
        Some(other) => {
            return Err(safety::bad_arg(
                "proc_run",
                ENCODING_EXPECTED,
                other.type_name(),
            ))
        }
    };

    let capture = match options.get("capture") {
        None => Capture::Prefix,
        Some(Value::Str(label)) => Capture::parse(label).ok_or_else(|| {
            safety::bad_arg(
                "proc_run",
                CAPTURE_EXPECTED,
                &format!("the label {label:?}"),
            )
        })?,
        Some(other) => {
            return Err(safety::bad_arg(
                "proc_run",
                CAPTURE_EXPECTED,
                other.type_name(),
            ))
        }
    };

    let on_limit = match options.get("on_output_limit") {
        None => OnLimit::Terminate,
        Some(Value::Str(label)) => OnLimit::parse(label).ok_or_else(|| {
            safety::bad_arg(
                "proc_run",
                ON_LIMIT_EXPECTED,
                &format!("the label {label:?}"),
            )
        })?,
        Some(other) => {
            return Err(safety::bad_arg(
                "proc_run",
                ON_LIMIT_EXPECTED,
                other.type_name(),
            ))
        }
    };

    let timeout = int_option(
        options.get("timeout_ms"),
        "timeout_ms to be an Int from 1 to 600000",
        DEFAULT_TIMEOUT_MS,
        MAX_TIMEOUT_MS,
    )?;
    let max_output = int_option(
        options.get("max_output_bytes"),
        "max_output_bytes to be an Int from 1 to 16777216",
        DEFAULT_MAX_OUTPUT_BYTES,
        MAX_OUTPUT_BYTES,
    )?;
    let max_log = int_option(
        options.get("max_log_bytes"),
        "max_log_bytes to be an Int from 1 to 1073741824",
        DEFAULT_MAX_LOG_BYTES,
        MAX_LOG_BYTES,
    )?;

    let cwd = match options.get("cwd") {
        None => None,
        Some(Value::Str(text)) => {
            if text.is_empty() {
                return Err(safety::bad_arg(
                    "proc_run",
                    "cwd to be a non-empty String",
                    "an empty String",
                ));
            }
            if text.contains('\0') {
                return Err(safety::bad_arg(
                    "proc_run",
                    "cwd to contain no NUL",
                    "a String containing U+0000",
                ));
            }
            Some(text.clone())
        }
        Some(other) => {
            return Err(safety::bad_arg(
                "proc_run",
                "cwd to be a non-empty String",
                other.type_name(),
            ))
        }
    };

    let env = match options.get("env") {
        None => BTreeMap::new(),
        Some(Value::Record(entries)) => parse_env(entries)?,
        Some(other) => {
            return Err(safety::bad_arg(
                "proc_run",
                "env to be a Record of String names to String values or null",
                other.type_name(),
            ))
        }
    };

    let stdin = match (options.get("stdin"), options.get("stdin_base64")) {
        (Some(_), Some(_)) => {
            return Err(safety::bad_arg(
                "proc_run",
                "at most one of stdin and stdin_base64",
                "both",
            ))
        }
        (Some(Value::Str(text)), None) => Some(text.clone().into_bytes()),
        (Some(other), None) => {
            return Err(safety::bad_arg(
                "proc_run",
                "stdin to be a String",
                other.type_name(),
            ))
        }
        (None, Some(Value::Str(text))) => {
            use base64::{engine::general_purpose::STANDARD, Engine as _};
            Some(STANDARD.decode(text).map_err(|_| {
                safety::bad_arg(
                    "proc_run",
                    "stdin_base64 to be standard base64",
                    "a value that is not base64",
                )
            })?)
        }
        (None, Some(other)) => {
            return Err(safety::bad_arg(
                "proc_run",
                "stdin_base64 to be a String",
                other.type_name(),
            ))
        }
        (None, None) => None,
    };
    if let Some(bytes) = stdin.as_ref() {
        if bytes.len() > MAX_STDIN_BYTES {
            return Err(safety::bad_arg(
                "proc_run",
                "stdin no longer than 16777216 bytes",
                &format!("{} bytes", bytes.len()),
            ));
        }
    }

    Ok(Options {
        encoding,
        timeout: Duration::from_millis(timeout as u64),
        max_output: max_output as usize,
        capture,
        on_limit,
        cwd,
        env,
        stdin,
        logs: [
            log_option(options, "stdout_log")?,
            log_option(options, "stderr_log")?,
        ],
        max_log: max_log as usize,
    })
}

fn log_option(options: &BTreeMap<String, Value>, key: &str) -> Result<Option<String>> {
    match options.get(key) {
        None => Ok(None),
        Some(Value::Str(text)) => {
            if text.is_empty() {
                return Err(safety::bad_arg(
                    "proc_run",
                    &format!("{key} to be a non-empty String path"),
                    "an empty String",
                ));
            }
            if text.contains('\0') {
                return Err(safety::bad_arg(
                    "proc_run",
                    &format!("{key} to contain no NUL"),
                    "a String containing U+0000",
                ));
            }
            Ok(Some(text.clone()))
        }
        Some(other) => Err(safety::bad_arg(
            "proc_run",
            &format!("{key} to be a non-empty String path"),
            other.type_name(),
        )),
    }
}

/// Validate the child's environment overrides.
///
/// The names in the `DSH_*`, `AETHER_*` and `AE_*` namespaces are the
/// deployment's own; a child that took them over would make the shell's own
/// controls unreliable, so they are refused as a convenience rather than as a
/// boundary — an `argv` can still run a program that sets them for itself.
fn parse_env(entries: &BTreeMap<String, Value>) -> Result<BTreeMap<String, Option<String>>> {
    let mut env = BTreeMap::new();
    #[cfg(windows)]
    let mut folded: Vec<String> = Vec::new();

    for (name, value) in entries {
        if name.is_empty() {
            return Err(safety::bad_arg(
                "proc_run",
                "env names to be non-empty",
                "an empty name",
            ));
        }
        if name.contains('=') {
            return Err(safety::bad_arg(
                "proc_run",
                "env names to contain no '='",
                &format!("the name {name:?}"),
            ));
        }
        if name.contains('\0') {
            return Err(safety::bad_arg(
                "proc_run",
                "env names to contain no NUL",
                &format!("the name {name:?}"),
            ));
        }
        let upper = name.to_ascii_uppercase();
        if upper.starts_with("DSH_") || upper.starts_with("AETHER_") || upper.starts_with("AE_") {
            return Err(safety::bad_arg(
                "proc_run",
                "env names outside the DSH_*, AETHER_* and AE_* namespaces",
                &format!("the managed name {name:?}"),
            ));
        }
        #[cfg(windows)]
        {
            if folded.contains(&upper) {
                return Err(safety::bad_arg(
                    "proc_run",
                    "env names that differ by more than case on Windows",
                    &format!("the duplicate name {name:?}"),
                ));
            }
            folded.push(upper);
        }

        let value = match value {
            Value::Str(text) => {
                if text.contains('\0') {
                    return Err(safety::bad_arg(
                        "proc_run",
                        "env values to contain no NUL",
                        &format!("a NUL in the value for {name:?}"),
                    ));
                }
                Some(text.clone())
            }
            Value::Null => None,
            other => {
                return Err(safety::bad_arg(
                    "proc_run",
                    "env values to be Strings or null",
                    other.type_name(),
                ))
            }
        };
        env.insert(name.clone(), value);
    }
    Ok(env)
}

fn int_option(value: Option<&Value>, expected: &str, default: i64, max: i64) -> Result<i64> {
    let number = match value {
        None => default,
        Some(Value::Int(number)) => *number,
        Some(other) => return Err(safety::bad_arg("proc_run", expected, other.type_name())),
    };
    if !(1..=max).contains(&number) {
        return Err(safety::bad_arg("proc_run", expected, &number.to_string()));
    }
    Ok(number)
}

// ── resolution ──────────────────────────────────────────────────────────

fn interpreter_cwd() -> Result<PathBuf> {
    std::env::current_dir().map_err(|error| {
        anyhow::Error::new(SafetyError {
            code: ErrorCode::Io,
            message: format!("proc_run: cannot read the interpreter's working directory: {error}"),
            builtin: "proc_run".to_string(),
            hint: "a working directory is needed to resolve a relative cwd or log path".to_string(),
            approval: None,
            did_you_mean: Vec::new(),
            expected: "a readable working directory".to_string(),
            got: error.kind().to_string(),
        })
    })
}

/// Canonicalize the caller's `cwd` against the interpreter's own directory.
///
/// A relative path is anchored to where the interpreter is, never to a
/// directory this call changes: the interpreter's working directory is a
/// process-global fact and is never written here.
fn resolve_cwd(cwd: &Option<String>) -> Result<Option<PathBuf>> {
    let Some(text) = cwd.as_deref() else {
        return Ok(None);
    };
    let base = interpreter_cwd()?;
    let candidate = Path::new(text);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    let canonical = std::fs::canonicalize(&joined).map_err(|error| {
        safety::bad_arg(
            "proc_run",
            "cwd to name an existing directory",
            &format!("{text:?} ({error})"),
        )
    })?;
    if !canonical.is_dir() {
        return Err(safety::bad_arg(
            "proc_run",
            "cwd to name an existing directory",
            &format!("{text:?} (not a directory)"),
        ));
    }
    // The canonical path is what the child is started in and what a relative log
    // path is resolved against, so a directory whose name cannot be represented
    // as text is refused here rather than silently lossy later.
    if canonical.to_str().is_none() {
        return Err(safety::bad_arg(
            "proc_run",
            "cwd to be representable as UTF-8",
            &format!("{text:?} (the directory name is not valid text)"),
        ));
    }
    Ok(Some(canonical))
}

/// Resolve both log targets against the child's effective working directory.
///
/// Nothing is created here: the parent directory must already exist, the file
/// itself must not, and the two paths must name different files. Both are
/// settled before a single byte is written or a process exists.
fn resolve_logs(logs: &[Option<String>; 2], cwd: Option<&Path>) -> Result<[Option<LogTarget>; 2]> {
    if logs[0].is_none() && logs[1].is_none() {
        return Ok([None, None]);
    }
    let base = match cwd {
        Some(dir) => dir.to_path_buf(),
        None => interpreter_cwd()?,
    };
    let mut targets: [Option<LogTarget>; 2] = [None, None];

    for index in 0..2 {
        let Some(text) = logs[index].as_deref() else {
            continue;
        };
        let stream = STREAMS[index];
        let expected = format!("{stream}_log to name a new file in an existing directory");
        let candidate = Path::new(text);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            base.join(candidate)
        };
        let name = match joined.file_name() {
            Some(name) if !name.is_empty() => name.to_os_string(),
            _ => {
                return Err(safety::bad_arg(
                    "proc_run",
                    &expected,
                    &format!("{text:?} (no file name)"),
                ))
            }
        };
        let parent = match joined.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => {
                return Err(safety::bad_arg(
                    "proc_run",
                    &expected,
                    &format!("{text:?} (no directory)"),
                ))
            }
        };
        let canonical_parent = std::fs::canonicalize(&parent).map_err(|error| {
            safety::bad_arg(
                "proc_run",
                &expected,
                &format!("{text:?} (the directory does not exist: {error})"),
            )
        })?;
        if !canonical_parent.is_dir() {
            return Err(safety::bad_arg(
                "proc_run",
                &expected,
                &format!("{text:?} (the parent is not a directory)"),
            ));
        }
        let path = canonical_parent.join(&name);
        if path.exists() {
            return Err(safety::bad_arg(
                "proc_run",
                &expected,
                &format!("{text:?} (the file already exists; a log is never overwritten)"),
            ));
        }
        // The guard target and every path in a result or a diagnostic come from
        // this exact string. A lossy conversion could name a different file than
        // the one the guard approved and the reader opens, so an unrepresentable
        // path is refused before anything exists.
        let exact = match path.to_str() {
            Some(exact) => exact.to_string(),
            None => {
                return Err(safety::bad_arg(
                    "proc_run",
                    &expected,
                    &format!("{text:?} (the path is not valid text)"),
                ))
            }
        };
        targets[index] = Some(LogTarget {
            display: crate::path_display::display_path(&path),
            guard_target: guard_path(&exact),
            path,
        });
    }

    if let (Some(left), Some(right)) = (targets[0].as_ref(), targets[1].as_ref()) {
        if same_path(&left.path, &right.path) {
            return Err(safety::bad_arg(
                "proc_run",
                "stdout_log and stderr_log to name different files",
                "the same file twice",
            ));
        }
    }
    Ok(targets)
}

/// Render a canonical path for guard targets, results and diagnostics.
///
/// The input is the exact checked string of the path that is opened, never a
/// lossy conversion of it. Windows canonicalization answers in the verbatim
/// `\\?\` namespace; the guard's lexical audit-artifact check compares plain
/// text, so that prefix is removed for the strings that reach it, and an
/// ordinary Windows path is returned unchanged.
fn guard_path(exact: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(rest) = exact.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = exact.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    exact.to_string()
}

#[cfg(windows)]
fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.to_string_lossy();
    let right = right.to_string_lossy();
    left.eq_ignore_ascii_case(right.as_ref())
}

#[cfg(not(windows))]
fn same_path(left: &Path, right: &Path) -> bool {
    left == right
}

/// Gate the log targets as one `WriteLocal` effect of this call.
///
/// The guard runs before `execute` opens anything, so a denied path leaves no
/// file and starts no process, and it cannot be bypassed by naming the same
/// destination another way: the target is the canonicalized path.
fn guard_log_writes(request: &Request) -> Result<()> {
    let targets: Vec<String> = request
        .logs
        .iter()
        .flatten()
        .map(|log| log.guard_target.clone())
        .collect();
    if targets.is_empty() {
        return Ok(());
    }
    safety::guard(GuardCtx {
        builtin: "proc_run",
        effect: Effect::WriteLocal,
        what: "write",
        targets,
        blast_radius: serde_json::json!({ "bytes": request.max_log }),
        reversible: false,
        fs_paths: true,
    })?;
    Ok(())
}

/// The command string the exec audit entry records: every argument rendered with
/// Rust's string escaping, so an argument containing a space, a quote or a
/// newline cannot read as a second argument or a second line. Bounded, so an
/// argv of any size cannot flood the log. Environment *values* are never
/// included.
fn audit_command(argv: &[String]) -> String {
    const LIMIT: usize = 4096;
    let mut out = String::new();
    for (index, arg) in argv.iter().enumerate() {
        let rendered = format!("{arg:?}");
        if index > 0 {
            out.push(' ');
        }
        let budget = LIMIT.saturating_sub(out.len());
        if rendered.len() <= budget {
            out.push_str(&rendered);
        } else {
            let mut end = budget.min(rendered.len());
            while end > 0 && !rendered.is_char_boundary(end) {
                end -= 1;
            }
            out.push_str(&rendered[..end]);
            out.push_str(" ...");
            break;
        }
    }
    out
}

// ── execution ───────────────────────────────────────────────────────────

/// The bytes one reader captured, how much it read, and how the capture ended.
///
/// The reader updates this under a shared lock as it reads, so what it has
/// already read is available even if it never reaches the end of the stream.
#[derive(Clone, Default)]
struct StreamOutcome {
    /// The retained bytes: the prefix or the tail of the stream.
    data: VecDeque<u8>,
    /// The stream was longer than the caller's limit.
    truncated: bool,
    /// Every byte read from this pipe so far, exact.
    total_bytes: u64,
    /// The read itself failed; the bytes above are what arrived before that.
    error: Option<String>,
    /// The log this stream was written to, when the caller asked for one.
    log: Option<LogOutcome>,
}

/// What one log file received.
#[derive(Clone, Default)]
struct LogOutcome {
    path: String,
    bytes: u64,
    complete: bool,
    error: Option<String>,
}

/// What happened to the child's standard input.
#[derive(Clone)]
struct StdinOutcome {
    requested: bool,
    bytes: u64,
    written_bytes: u64,
    complete: bool,
    error: Option<String>,
}

impl Default for StdinOutcome {
    fn default() -> Self {
        // No stdin was requested, and there is nothing left to write.
        Self {
            requested: false,
            bytes: 0,
            written_bytes: 0,
            complete: true,
            error: None,
        }
    }
}

/// The capture a reader builds, shared with the caller.
type SharedCapture = Arc<Mutex<StreamOutcome>>;

/// Everything one reader needs.
struct ReaderPlan {
    index: usize,
    cap: usize,
    capture: Capture,
    log: Option<LogSink>,
}

/// The log a reader owns while it runs.
struct LogSink {
    file: File,
    cap: usize,
    path: String,
}

fn lock_capture(shared: &Mutex<StreamOutcome>) -> MutexGuard<'_, StreamOutcome> {
    // A panic in a reader must not turn a partial capture into an error about
    // the lock; the bytes already stored are still the answer.
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_stdin(shared: &Mutex<StdinOutcome>) -> MutexGuard<'_, StdinOutcome> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct GroupGuard {
    child: Box<dyn ChildWrapper>,
    /// Cleared once the caller has killed the group and reaped the leader, so
    /// the drop guard does not repeat the work.
    armed: bool,
}

impl GroupGuard {
    fn new(child: Box<dyn ChildWrapper>) -> Self {
        Self { child, armed: true }
    }

    /// The leader's status, or `None` while it is still running.
    ///
    /// The group child's `try_wait` polls the managed group and then the leader,
    /// so it answers for the leader whether or not a descendant is still alive —
    /// which is what the deadline needs to see. Genuine progress is cached by
    /// the dependency, and an error is not a verdict on the child, so it is
    /// retried on the next tick rather than reported.
    fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Request termination of everything in the managed group, without waiting.
    ///
    /// `true` means the operating system accepted the request, or the group was
    /// already gone (an empty process group reports ESRCH). It is an
    /// acknowledgement, not settlement: this returns before the group is known
    /// to be empty, and nothing available here can prove that without blocking.
    fn kill_group(&mut self) -> bool {
        match self.child.start_kill() {
            Ok(()) => true,
            // An empty process group reports ESRCH, which is the answer the
            // caller wants rather than a failure.
            Err(ref error) if already_gone(error) => true,
            Err(_) => false,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Reached only from an early return or a panic: this call must not leave
        // a process running that it started, and the leader must not stay a
        // zombie for the life of the shell. Bounded, because a hang here would
        // hang the caller.
        self.kill_group();
        let end = Instant::now() + CLEANUP_GRACE;
        while self.try_wait().is_none() && Instant::now() < end {
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(unix)]
fn already_gone(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn already_gone(_error: &std::io::Error) -> bool {
    false
}

fn execute(mut request: Request) -> Result<Value> {
    let program = request.argv[0].clone();
    // `image` is the path validation resolved and checked on Windows; elsewhere
    // the program name is passed through unchanged.
    #[cfg(windows)]
    let image = request.image.clone();
    #[cfg(not(windows))]
    let image: std::ffi::OsString = program.clone().into();

    // The logs are created before the child exists, each exclusively. If the
    // second cannot be created the first may already exist and be empty; that is
    // said plainly rather than pretended away, and no process was started.
    let mut files: [Option<File>; 2] = [None, None];
    let mut created: Vec<String> = Vec::new();
    for index in 0..2 {
        let Some(target) = request.logs[index].as_ref() else {
            continue;
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target.path)
        {
            Ok(file) => {
                created.push(target.display.clone());
                files[index] = Some(file);
            }
            Err(error) => return Err(log_open_error(target, &error, &created)),
        }
    }

    let mut stdin_data = request.stdin.take();
    let piped_stdin = stdin_data.is_some();
    let cwd = request.cwd.clone();
    let env = request.env.clone();

    let mut wrap = CommandWrap::with_new(&image, |command| {
        command
            .args(&request.argv[1..])
            .stdin(if piped_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // A validated, canonical directory only, and never the interpreter's.
        if let Some(dir) = cwd.as_deref() {
            command.current_dir(dir);
        }
        // The child's environment only: every other inherited name, including an
        // ambient credential the deployment did not scrub, stays as it was.
        for (name, value) in &env {
            match value {
                Some(value) => {
                    command.env(name, value);
                }
                None => {
                    command.env_remove(name);
                }
            }
        }
    });
    // The managed group: a POSIX process group on Unix, a job object on Windows.
    #[cfg(unix)]
    wrap.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    wrap.wrap(JobObject);
    let mut child = wrap.spawn().map_err(|error| {
        // A spawn failure means no child ran and the logs created a moment ago
        // are empty, but they still exist and are named.
        with_created_logs(
            safety::spawn_error("proc_run", &program, &error),
            &created,
            false,
        )
    })?;
    // A spawn failure is reported as a structured error and nothing is waited
    // for: there is no handle for a process that could not be started, so any
    // cleanup of a half-created group belongs to the spawn implementation.

    // Both pipes are taken before anything waits on the child: reading stdout to
    // end before touching stderr deadlocks a child that fills stderr first, and
    // a reader thread per stream avoids that shape entirely. The stdin pipe is
    // taken here too, so the writer can run at the same time as both readers.
    let stdout_pipe = child
        .stdout()
        .take()
        .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
    let stderr_pipe = child
        .stderr()
        .take()
        .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
    let stdin_pipe = child
        .stdin()
        .take()
        .map(|pipe| Box::new(pipe) as Box<dyn Write + Send>);
    let sources: [Option<Box<dyn Read + Send>>; 2] = [stdout_pipe, stderr_pipe];

    let mut guard = GroupGuard::new(child);

    let limit = Arc::new(AtomicBool::new(false));
    let log_stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel::<(usize, StreamOutcome)>();
    let mut handles: [Option<JoinHandle<()>>; 2] = [None, None];
    let mut captures: [Option<SharedCapture>; 2] = [None, None];
    let mut outcomes: [Option<StreamOutcome>; 2] = [None, None];
    let mut root: Option<ExitStatus> = None;
    let mut writer: Option<JoinHandle<()>> = None;
    let mut writer_state: Arc<Mutex<StdinOutcome>> = Arc::new(Mutex::new(StdinOutcome::default()));

    for (index, source) in sources.into_iter().enumerate() {
        let Some(source) = source else { continue };
        let plan = ReaderPlan {
            index,
            cap: request.max_output,
            capture: request.capture,
            log: files[index].take().map(|file| LogSink {
                file,
                cap: request.max_log,
                path: request.logs[index]
                    .as_ref()
                    .map(|target| target.display.clone())
                    .unwrap_or_default(),
            }),
        };
        let spawned = spawn_reader(
            source,
            plan,
            Arc::clone(&limit),
            Arc::clone(&log_stop),
            sender.clone(),
        );
        match spawned {
            Ok((handle, capture)) => {
                handles[index] = Some(handle);
                captures[index] = Some(capture);
            }
            Err(error) => {
                // No reader took this pipe, and the reader already started holds
                // the other one. Kill the group so no child writes into a pipe
                // nobody drains, then settle before reporting, so a half-started
                // capture cannot outlive the call.
                guard.kill_group();
                settle(
                    &mut guard,
                    &mut handles,
                    &mut outcomes,
                    &receiver,
                    &mut root,
                    &mut writer,
                    Instant::now() + CLEANUP_GRACE,
                );
                guard.disarm();
                return Err(with_created_logs(
                    thread_error(&program, "output reader", &error),
                    &created,
                    true,
                ));
            }
        }
    }
    drop(sender);

    if let Some(bytes) = stdin_data.take() {
        let length = bytes.len() as u64;
        match stdin_pipe {
            Some(sink) => {
                let shared = Arc::new(Mutex::new(StdinOutcome {
                    requested: true,
                    bytes: length,
                    written_bytes: 0,
                    complete: false,
                    error: None,
                }));
                match spawn_writer(sink, Arc::new(bytes), Arc::clone(&shared)) {
                    Ok(handle) => {
                        writer = Some(handle);
                        writer_state = shared;
                    }
                    Err(error) => {
                        guard.kill_group();
                        settle(
                            &mut guard,
                            &mut handles,
                            &mut outcomes,
                            &receiver,
                            &mut root,
                            &mut writer,
                            Instant::now() + CLEANUP_GRACE,
                        );
                        guard.disarm();
                        return Err(with_created_logs(
                            thread_error(&program, "stdin writer", &error),
                            &created,
                            true,
                        ));
                    }
                }
            }
            None => {
                writer_state = Arc::new(Mutex::new(StdinOutcome {
                    requested: true,
                    bytes: length,
                    written_bytes: 0,
                    complete: false,
                    error: Some(
                        "the child's stdin pipe was not available, so none of the bytes were sent"
                            .to_string(),
                    ),
                }));
            }
        }
    }

    let deadline = Instant::now() + request.timeout;
    let mut timed_out = false;
    let mut stop_at: Option<Instant> = None;

    loop {
        drain(&receiver, &mut outcomes);
        join_finished(&mut handles, &mut outcomes, &receiver);
        join_writer(&mut writer);
        if root.is_none() {
            root = guard.try_wait();
        }
        // A leader that exited is not the end of the call: the captures must be
        // closed too, or a descendant is still writing into them, and a writer
        // still blocked on stdin is still this call's thread.
        if root.is_some() && captured(&outcomes) && writer.is_none() {
            break;
        }

        let now = Instant::now();
        if let Some(end) = stop_at {
            if now >= end {
                break;
            }
        } else if now >= deadline {
            timed_out = true;
            stop_at = Some(now + CLEANUP_GRACE);
            guard.kill_group();
        } else if log_stop.load(Ordering::Relaxed) {
            // A log that cannot be completed is a failed part of this call; the
            // reader keeps draining, and the group is asked to stop so the call
            // does not run on producing bytes nobody can keep.
            stop_at = Some(now + CLEANUP_GRACE);
            guard.kill_group();
        } else if limit.load(Ordering::Relaxed) && request.on_limit == OnLimit::Terminate {
            // The caller asked for at most this much, and the reader is already
            // discarding the rest; killing the group stops the writer.
            stop_at = Some(now + CLEANUP_GRACE);
            guard.kill_group();
        }

        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok((index, outcome)) => outcomes[index] = Some(outcome),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => thread::sleep(POLL_INTERVAL),
        }
    }

    // Every path ends by requesting termination of the managed group, so a
    // descendant that outlived the leader — and would otherwise keep holding
    // these pipes — is reached. On the normal path this request is the only
    // thing that reaches it, since the leader is already reaped.
    let termination_requested = guard.kill_group();
    settle(
        &mut guard,
        &mut handles,
        &mut outcomes,
        &receiver,
        &mut root,
        &mut writer,
        stop_at.unwrap_or_else(|| Instant::now() + CLEANUP_GRACE),
    );
    guard.disarm();

    // One last look at the readers that finished while the bound ran out: a
    // joined reader has already queued its result, so this is what tells a
    // completed capture apart from one that is still blocked.
    join_finished(&mut handles, &mut outcomes, &receiver);
    join_writer(&mut writer);

    // A reader that is still blocked on a pipe the process holds has stored the
    // prefix it read but never reported it. Answering "no bytes available" would
    // throw that prefix away, so the shared capture is read instead and the
    // stream is marked as what it is: a partial capture of an open stream.
    for (index, capture) in captures.iter().enumerate() {
        if outcomes[index].is_some() {
            continue;
        }
        let Some(capture) = capture else { continue };
        let mut partial = lock_capture(capture).clone();
        if partial.error.is_none() {
            partial.error = Some(
                "the capture is still open: the process still holds this pipe, so `bytes` \
                 counts the part read so far and `complete` is false"
                    .to_string(),
            );
        }
        outcomes[index] = Some(partial);
    }

    let stdout = stream_report(request.encoding, request.capture, outcomes[0].as_ref());
    let stderr = stream_report(request.encoding, request.capture, outcomes[1].as_ref());
    let stdin = lock_stdin(&writer_state).clone();
    let process_success = root.as_ref().is_some_and(|status| status.success());
    // The limit fact comes from the flag as well as from the captures: a reader
    // that discarded bytes and then never reported still means the caller's
    // limit was exceeded.
    let output_limit_exceeded =
        limit.load(Ordering::Relaxed) || stdout.truncated || stderr.truncated;
    let decoded = match request.encoding {
        Choice::Bytes => true,
        _ => stdout.decoded == Some(true) && stderr.decoded == Some(true),
    };
    // A log that is short of the stream is a lost artifact, so it is part of the
    // same verdict; with no log requested this is vacuously true.
    let logs_complete = stdout.log.as_ref().is_none_or(|log| log.complete)
        && stderr.log.as_ref().is_none_or(|log| log.complete);
    // Narrow on purpose: the leader was reaped and every reader and writer this
    // call started was joined. Handles are `None` only after `join`, so a thread
    // that had to be detached — a descendant outside the managed group can hold
    // a pipe open — leaves this false and an incomplete capture behind.
    let cleanup_complete =
        root.is_some() && handles[0].is_none() && handles[1].is_none() && writer.is_none();
    // A requested stdin that was not fully accepted is a lost part of the call,
    // so it is part of the same verdict. An absent `stdin` is initialized
    // complete, so this changes nothing for a call that sent none.
    let success = process_success
        && !timed_out
        && !output_limit_exceeded
        && termination_requested
        && cleanup_complete
        && stdout.complete
        && stderr.complete
        && logs_complete
        && stdin.complete
        && decoded;

    let mut process = BTreeMap::new();
    process.insert("success".to_string(), Value::Bool(process_success));
    process.insert(
        "exit_code".to_string(),
        root.as_ref()
            .and_then(|status| status.code())
            .map(|code| Value::Int(code as i64))
            .unwrap_or(Value::Null),
    );
    process.insert("timed_out".to_string(), Value::Bool(timed_out));
    process.insert(
        "output_limit_exceeded".to_string(),
        Value::Bool(output_limit_exceeded),
    );
    process.insert(
        "cleanup_complete".to_string(),
        Value::Bool(cleanup_complete),
    );
    process.insert(
        "group_termination_requested".to_string(),
        Value::Bool(termination_requested),
    );

    let mut record = BTreeMap::new();
    record.insert("success".to_string(), Value::Bool(success));
    record.insert("process".to_string(), Value::Record(process));
    record.insert("stdin".to_string(), stdin_value(&stdin));
    record.insert("stdout".to_string(), stdout.to_value());
    record.insert("stderr".to_string(), stderr.to_value());
    Ok(Value::Record(record))
}

/// Read one stream into a bounded capture, tee the raw bytes to a log when one
/// was requested, and keep draining to end of stream.
///
/// Draining matters: stopping at the limit would leave the child blocked on a
/// full pipe and turn a bounded capture into a hang. What is discarded is
/// counted by `truncated` and by `total_bytes`, which is the honest statement
/// that the capture is partial — the total the child produced is never guessed.
///
/// Every chunk is stored in `shared` as it is read, for two reasons: what has
/// been read survives a reader that never reaches the end of the stream, and the
/// caller can read it at any time without waiting for this thread to finish.
fn read_stream(
    mut source: Box<dyn Read + Send>,
    plan: ReaderPlan,
    limit: &AtomicBool,
    log_stop: &AtomicBool,
    shared: &Mutex<StreamOutcome>,
) {
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut sink = plan.log;
    let mut log_bytes: u64 = 0;
    let mut log_error: Option<String> = None;

    loop {
        match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let chunk = &buffer[..read];

                // The log receives the raw bytes exactly as they were read. On a
                // cap or a write failure the file is left as it is, the failure
                // is recorded, and the loop keeps draining so the child cannot
                // block on a full pipe.
                if let Some(sink) = sink.as_mut() {
                    if log_error.is_none() {
                        let room = sink.cap.saturating_sub(log_bytes as usize);
                        let take = read.min(room);
                        if take > 0 {
                            let (written, failure) = write_counting(&mut sink.file, &chunk[..take]);
                            log_bytes += written as u64;
                            if let Some(failure) = failure {
                                let stream = STREAMS[plan.index];
                                log_error =
                                    Some(format!("could not write {stream}_log: {failure}"));
                                log_stop.store(true, Ordering::Relaxed);
                            }
                        }
                        if log_error.is_none() && take < read {
                            let stream = STREAMS[plan.index];
                            let cap = sink.cap;
                            log_error = Some(format!(
                                "{stream}_log reached max_log_bytes ({cap}): only the first \
                                 {log_bytes} bytes were written"
                            ));
                            log_stop.store(true, Ordering::Relaxed);
                        }
                    }
                }

                let mut captured = lock_capture(shared);
                captured.total_bytes += read as u64;
                match plan.capture {
                    Capture::Prefix => {
                        let room = plan.cap.saturating_sub(captured.data.len());
                        if read <= room {
                            captured.data.extend(chunk.iter().copied());
                        } else {
                            captured.data.extend(chunk[..room].iter().copied());
                            captured.truncated = true;
                            limit.store(true, Ordering::Relaxed);
                        }
                    }
                    Capture::Tail => {
                        // A bounded ring: only the most recent `cap` bytes are
                        // kept. A chunk that is already larger than the ring
                        // replaces it outright; otherwise the oldest bytes go,
                        // which is one head advance rather than a shift per
                        // byte.
                        if read >= plan.cap {
                            captured.data.clear();
                            captured
                                .data
                                .extend(chunk[read - plan.cap..].iter().copied());
                        } else {
                            let overflow = (captured.data.len() + read).saturating_sub(plan.cap);
                            if overflow > 0 {
                                captured.data.drain(0..overflow);
                            }
                            captured.data.extend(chunk.iter().copied());
                        }
                        if captured.total_bytes > plan.cap as u64 {
                            captured.truncated = true;
                            limit.store(true, Ordering::Relaxed);
                        }
                    }
                }
                if let Some(log) = captured.log.as_mut() {
                    log.bytes = log_bytes;
                    log.error = log_error.clone();
                }
            }
            Err(ref failure) if failure.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(failure) => {
                let mut captured = lock_capture(shared);
                captured.error = Some(format!("could not read the stream: {failure}"));
                break;
            }
        }
    }

    if let Some(mut sink) = sink {
        if log_error.is_none() {
            if let Err(failure) = sink.file.flush() {
                log_error = Some(format!("could not flush the log: {failure}"));
            } else if let Err(failure) = sink.file.sync_all() {
                log_error = Some(format!("could not sync the log to disk: {failure}"));
            }
        }
        let mut captured = lock_capture(shared);
        let total_bytes = captured.total_bytes;
        if log_error.is_none() {
            log_error = captured
                .error
                .clone()
                .map(|error| format!("the source stream is incomplete: {error}"));
        }
        if let Some(log) = captured.log.as_mut() {
            log.path = sink.path;
            log.bytes = log_bytes;
            log.complete = log_error.is_none() && log_bytes == total_bytes;
            log.error = log_error;
        }
    }
}

/// Write every byte, reporting how many the file accepted before a failure.
///
/// A short write is normal, so this loops; a partial write followed by an error
/// is reported as what it is instead of being treated as all-or-nothing.
fn write_counting(sink: &mut File, bytes: &[u8]) -> (usize, Option<std::io::Error>) {
    let mut written = 0;
    while written < bytes.len() {
        match sink.write(&bytes[written..]) {
            Ok(0) => {
                return (
                    written,
                    Some(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "the log stopped accepting bytes",
                    )),
                )
            }
            Ok(count) => written += count,
            Err(ref error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return (written, Some(error)),
        }
    }
    (written, None)
}

fn spawn_reader(
    source: Box<dyn Read + Send>,
    plan: ReaderPlan,
    limit: Arc<AtomicBool>,
    log_stop: Arc<AtomicBool>,
    sender: Sender<(usize, StreamOutcome)>,
) -> std::io::Result<(JoinHandle<()>, SharedCapture)> {
    let index = plan.index;
    let mut initial = StreamOutcome::default();
    if let Some(sink) = plan.log.as_ref() {
        initial.log = Some(LogOutcome {
            path: sink.path.clone(),
            bytes: 0,
            complete: false,
            error: None,
        });
    }
    let shared: SharedCapture = Arc::new(Mutex::new(initial));
    let reader_state = Arc::clone(&shared);
    let handle = thread::Builder::new()
        .name(format!("proc_run.{}", STREAMS[index]))
        .spawn(move || {
            read_stream(source, plan, &limit, &log_stop, &reader_state);
            // A clone, not a move: taking the shared capture here would leave a
            // gap in which a chunk read after the take is lost, and the caller
            // may be reading the same capture at this moment.
            let snapshot = lock_capture(&reader_state).clone();
            // The caller holds the receiver until every reader is joined or
            // detached, so this send cannot be observed as a failure.
            let _ = sender.send((index, snapshot));
        })?;
    Ok((handle, shared))
}

/// Feed the child's standard input while both readers drain.
///
/// The writer closes the pipe when it returns, which is the child's end of
/// input; what it reports is how many bytes the operating-system pipe accepted,
/// not how many the child consumed.
fn spawn_writer(
    mut sink: Box<dyn Write + Send>,
    bytes: Arc<Vec<u8>>,
    shared: Arc<Mutex<StdinOutcome>>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("proc_run.stdin".to_string())
        .spawn(move || {
            let mut written = 0usize;
            let mut error: Option<String> = None;
            while written < bytes.len() {
                match sink.write(&bytes[written..]) {
                    Ok(0) => {
                        error = Some("the child's stdin stopped accepting bytes".to_string());
                        break;
                    }
                    Ok(count) => {
                        written += count;
                        lock_stdin(&shared).written_bytes = written as u64;
                    }
                    Err(ref failure) if failure.kind() == std::io::ErrorKind::Interrupted => {
                        continue
                    }
                    Err(failure) => {
                        error = Some(match failure.kind() {
                            std::io::ErrorKind::BrokenPipe => {
                                "the child closed stdin before every byte was written".to_string()
                            }
                            _ => format!("could not write to the child's stdin: {failure}"),
                        });
                        break;
                    }
                }
            }
            if error.is_none() {
                if let Err(failure) = sink.flush() {
                    error = Some(format!("could not flush the child's stdin: {failure}"));
                }
            }
            let mut state = lock_stdin(&shared);
            state.written_bytes = written as u64;
            state.complete = error.is_none() && written == bytes.len();
            state.error = error;
        })
}

fn log_open_error(target: &LogTarget, error: &std::io::Error, created: &[String]) -> anyhow::Error {
    let hint = if created.is_empty() {
        "no process was started".to_string()
    } else {
        let created = created.join(", ");
        format!("no process was started; {created} was created empty before this failure")
    };
    let path = target.display.as_str();
    anyhow::Error::new(SafetyError {
        code: ErrorCode::Io,
        message: format!("proc_run: cannot create the log {path}: {error}"),
        builtin: "proc_run".to_string(),
        hint,
        approval: None,
        did_you_mean: Vec::new(),
        expected: "a new log file that does not exist yet".to_string(),
        got: error.kind().to_string(),
    })
}

fn thread_error(program: &str, what: &str, error: &std::io::Error) -> anyhow::Error {
    anyhow::Error::new(SafetyError {
        code: ErrorCode::Io,
        message: format!("proc_run: cannot start the {what} thread for `{program}`: {error}"),
        builtin: "proc_run".to_string(),
        hint: "the host refused a capture thread; group termination and bounded cleanup were \
               requested, and this infrastructure failure returns no result"
            .to_string(),
        approval: None,
        did_you_mean: Vec::new(),
        expected: "one reader thread per stream and one writer for stdin".to_string(),
        got: error.kind().to_string(),
    })
}

/// Name the log files a later failure left behind, keeping the coded error.
///
/// A failure after the logs exist must not read as if nothing happened: the
/// files are real, and only the caller can decide what to do with them, so they
/// are named in the hint and never deleted here. The wrapper is deliberately not
/// an `anyhow` context, which would replace the code with `E_UNKNOWN`: the
/// `SafetyError` is downcast, its hint is extended, and the same error returns.
fn with_created_logs(
    error: anyhow::Error,
    created: &[String],
    child_may_have_run: bool,
) -> anyhow::Error {
    if created.is_empty() {
        return error;
    }
    let logs = created.join(", ");
    let note = if child_may_have_run {
        format!(
            "log files were created before this failure; inspect them — they may hold output, \
             and the child may have run: {logs}"
        )
    } else {
        format!("log files were created empty before this failure and no child was started: {logs}")
    };
    match error.downcast::<SafetyError>() {
        Ok(mut safety) => {
            let hint = safety.hint;
            safety.hint = format!("{hint}; {note}");
            anyhow::Error::new(safety)
        }
        Err(error) => error,
    }
}

/// Wait, bounded, for the captures to close, the leader to be reaped, and a
/// stdin writer to finish; then join the threads that finished and detach the
/// ones that did not.
///
/// Detaching is the point of the bound: a descendant that left the managed group
/// can hold a pipe open indefinitely, and a detached thread cannot block the
/// caller or keep the process alive. The stream it was reading is reported
/// incomplete instead, which is what happened. Such a thread is the one resource
/// this call can leave behind, and only when a pipe outlived the bound; a thread
/// that cannot be started at all is reported as a coded error rather than as a
/// cleanup flag.
fn settle(
    guard: &mut GroupGuard,
    handles: &mut [Option<JoinHandle<()>>; 2],
    outcomes: &mut [Option<StreamOutcome>; 2],
    receiver: &Receiver<(usize, StreamOutcome)>,
    root: &mut Option<ExitStatus>,
    writer: &mut Option<JoinHandle<()>>,
    end: Instant,
) {
    loop {
        drain(receiver, outcomes);
        join_finished(handles, outcomes, receiver);
        join_writer(writer);
        if root.is_none() {
            *root = guard.try_wait();
        }
        if captured(outcomes) && root.is_some() && writer.is_none() {
            break;
        }
        if Instant::now() >= end {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }

    for index in 0..2 {
        if outcomes[index].is_some() {
            if let Some(handle) = handles[index].take() {
                let _ = handle.join();
            }
        }
    }
    join_writer(writer);
}

fn drain(receiver: &Receiver<(usize, StreamOutcome)>, outcomes: &mut [Option<StreamOutcome>; 2]) {
    while let Ok((index, outcome)) = receiver.try_recv() {
        outcomes[index] = Some(outcome);
    }
}

/// Join the stdin writer once it has finished, and leave a still-blocked one for
/// the caller to report as an incomplete resource.
fn join_writer(writer: &mut Option<JoinHandle<()>>) {
    if writer.as_ref().is_some_and(|handle| handle.is_finished()) {
        if let Some(handle) = writer.take() {
            let _ = handle.join();
        }
    }
}

/// Take the results of the readers that have finished, and record the ones that
/// ended without reporting one: an empty stream would be a lie about the child.
fn join_finished(
    handles: &mut [Option<JoinHandle<()>>; 2],
    outcomes: &mut [Option<StreamOutcome>; 2],
    receiver: &Receiver<(usize, StreamOutcome)>,
) {
    for index in 0..2 {
        if outcomes[index].is_some() {
            continue;
        }
        let finished = handles[index]
            .as_ref()
            .is_some_and(|handle| handle.is_finished());
        if !finished {
            continue;
        }
        if let Some(handle) = handles[index].take() {
            let _ = handle.join();
        }
        // A joined sender has already queued its message — `send` puts it in the
        // channel before the thread returns — so one drain after the join
        // settles whether it reported. Reading liveness before that drain is a
        // race: a reader that sends between the caller's drain and this check
        // would be recorded as an empty stream and its bytes dropped.
        drain(receiver, outcomes);
        if outcomes[index].is_none() {
            outcomes[index] = Some(StreamOutcome {
                data: VecDeque::new(),
                truncated: false,
                total_bytes: 0,
                error: Some("the output reader ended without reporting what it read".to_string()),
                log: None,
            });
        }
    }
}

fn captured(outcomes: &[Option<StreamOutcome>; 2]) -> bool {
    outcomes[0].is_some() && outcomes[1].is_some()
}

// ── the record ──────────────────────────────────────────────────────────

struct StreamReport {
    encoding: &'static str,
    capture: &'static str,
    bytes: usize,
    total_bytes: u64,
    offset_bytes: u64,
    base64: String,
    text: Option<String>,
    decoded: Option<bool>,
    complete: bool,
    truncated: bool,
    error: Option<String>,
    log: Option<LogOutcome>,
}

impl StreamReport {
    fn to_value(&self) -> Value {
        let mut record = BTreeMap::new();
        record.insert(
            "encoding".to_string(),
            Value::Str(self.encoding.to_string()),
        );
        record.insert("capture".to_string(), Value::Str(self.capture.to_string()));
        record.insert("bytes".to_string(), Value::Int(self.bytes as i64));
        record.insert(
            "total_bytes".to_string(),
            Value::Int(self.total_bytes as i64),
        );
        record.insert(
            "offset_bytes".to_string(),
            Value::Int(self.offset_bytes as i64),
        );
        record.insert("base64".to_string(), Value::Str(self.base64.clone()));
        record.insert(
            "text".to_string(),
            self.text.clone().map(Value::Str).unwrap_or(Value::Null),
        );
        record.insert(
            "decoded".to_string(),
            self.decoded.map(Value::Bool).unwrap_or(Value::Null),
        );
        record.insert("complete".to_string(), Value::Bool(self.complete));
        record.insert(
            "error".to_string(),
            self.error.clone().map(Value::Str).unwrap_or(Value::Null),
        );
        record.insert(
            "log".to_string(),
            self.log.as_ref().map(log_value).unwrap_or(Value::Null),
        );
        Value::Record(record)
    }
}

fn log_value(log: &LogOutcome) -> Value {
    let mut record = BTreeMap::new();
    record.insert("path".to_string(), Value::Str(log.path.clone()));
    record.insert("bytes".to_string(), Value::Int(log.bytes as i64));
    record.insert("complete".to_string(), Value::Bool(log.complete));
    record.insert(
        "error".to_string(),
        log.error.clone().map(Value::Str).unwrap_or(Value::Null),
    );
    Value::Record(record)
}

fn stdin_value(outcome: &StdinOutcome) -> Value {
    let mut record = BTreeMap::new();
    record.insert("requested".to_string(), Value::Bool(outcome.requested));
    record.insert("bytes".to_string(), Value::Int(outcome.bytes as i64));
    record.insert(
        "written_bytes".to_string(),
        Value::Int(outcome.written_bytes as i64),
    );
    record.insert("complete".to_string(), Value::Bool(outcome.complete));
    record.insert(
        "error".to_string(),
        outcome.error.clone().map(Value::Str).unwrap_or(Value::Null),
    );
    Value::Record(record)
}

fn stream_report(
    choice: Choice,
    capture: Capture,
    outcome: Option<&StreamOutcome>,
) -> StreamReport {
    // A reader that never reported leaves nothing to decode. Answering with an
    // empty text would read as a stream that really was empty, which is why
    // `text` and `decoded` are null here rather than `""` and `true`.
    let Some(outcome) = outcome else {
        return StreamReport {
            encoding: choice.label(),
            capture: capture.label(),
            bytes: 0,
            total_bytes: 0,
            offset_bytes: 0,
            base64: String::new(),
            text: None,
            decoded: None,
            complete: false,
            truncated: false,
            error: Some(
                "no capture reader was available for this stream, so none of its bytes are \
                 available: `bytes` is 0, `base64` is empty, and `complete` is false"
                    .to_string(),
            ),
            log: None,
        };
    };

    let (head, tail) = outcome.data.as_slices();
    let mut data = Vec::with_capacity(outcome.data.len());
    data.extend_from_slice(head);
    data.extend_from_slice(tail);

    let (text, decoded, decode_error) = match choice.codec() {
        None => (None, None, None),
        Some(codec) => match codec.decode_without_bom_handling_and_without_replacement(&data) {
            Some(text) => (Some(text.into_owned()), Some(true), None),
            None => (
                None,
                Some(false),
                Some(format!(
                    "the {} captured bytes are not valid {}; `base64` holds them unchanged, \
                     and `encoding` names the codec that was tried",
                    data.len(),
                    codec.name()
                )),
            ),
        },
    };

    // Transport first, then the limit, then the content: a failed read or a
    // capture cut short is why a decode may also have failed, and naming the
    // decode first would send the caller after the wrong cause.
    let error = outcome
        .error
        .clone()
        .or_else(|| {
            outcome.truncated.then(|| match capture {
                Capture::Prefix => format!(
                    "only the first {} bytes were captured: the stream was longer, so the \
                     rest was discarded",
                    data.len()
                ),
                Capture::Tail => format!(
                    "only the last {} bytes were captured: the stream was longer, so the \
                     earlier bytes were discarded",
                    data.len()
                ),
            })
        })
        .or(decode_error);

    StreamReport {
        encoding: choice.label(),
        capture: capture.label(),
        bytes: data.len(),
        total_bytes: outcome.total_bytes,
        offset_bytes: match capture {
            Capture::Prefix => 0,
            Capture::Tail => outcome.total_bytes.saturating_sub(data.len() as u64),
        },
        base64: {
            use base64::{engine::general_purpose::STANDARD, Engine as _};
            STANDARD.encode(&data)
        },
        text,
        decoded,
        complete: !outcome.truncated && outcome.error.is_none(),
        truncated: outcome.truncated,
        error,
        log: outcome.log.clone(),
    }
}

// ── Windows: no implicit command interpreter ────────────────────────────

#[cfg(windows)]
fn script_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

#[cfg(windows)]
fn script_refusal(program: &str, candidate: &Path) -> anyhow::Error {
    anyhow::Error::new(SafetyError {
        code: ErrorCode::BadArg,
        message: format!(
            "proc_run: `{program}` resolves to {}, a Windows command script",
            candidate.display()
        ),
        builtin: "proc_run".to_string(),
        hint: "a .cmd or .bat file is run by cmd.exe, which would be the implicit shell this \
               builtin does not have; name the interpreter yourself — proc_run([\"cmd.exe\", \
               \"/c\", ...]) — if running one is what you mean"
            .to_string(),
        approval: None,
        did_you_mean: Vec::new(),
        expected: "a program image".to_string(),
        got: candidate.display().to_string(),
    })
}

/// Refuse a Windows command script before anything is started, and answer the
/// image to start.
///
/// `.cmd` and `.bat` files are interpreted by `cmd.exe`, so starting one *is* a
/// shell invocation — the one thing this builtin promises not to do. The name is
/// resolved with an explicit `PATH`/`PATHEXT` search before the decision,
/// because a bare `foo` can resolve to `foo.bat`; a literal `.cmd` is
/// refused even when no such file exists. The resolved path is what the caller
/// then starts, so the file that was inspected is the file that runs. A name
/// `which` cannot resolve is passed through as written, which is the behaviour
/// the spawn would have had anyway. Naming `cmd.exe` explicitly is the caller's
/// own choice and stays allowed.
#[cfg(windows)]
fn refuse_windows_script(program: &str) -> Result<std::ffi::OsString> {
    use std::ffi::OsString;

    let resolved = which::which(program).ok();
    let candidate: &Path = resolved.as_deref().unwrap_or_else(|| Path::new(program));
    if script_extension(candidate) {
        return Err(script_refusal(program, candidate));
    }
    Ok(resolved
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from(program)))
}

/// One environment variable as the child would see it: the caller's override
/// when one is given, otherwise the ambient value. Windows names are
/// case-insensitive, so both are compared that way.
#[cfg(windows)]
fn effective_var(name: &str, env: &BTreeMap<String, Option<String>>) -> Option<String> {
    let mut found: Option<Option<String>> = None;
    for (key, value) in env {
        if key.eq_ignore_ascii_case(name) {
            found = Some(value.clone());
        }
    }
    match found {
        Some(value) => value,
        None => std::env::var(name).ok(),
    }
}

/// Resolve and check the image to start against the child's own working
/// directory and environment.
///
/// The ambient environment is never rewritten for this: `PATH` and `PATHEXT`
/// come from the caller's overrides when given, and relative `PATH` entries are
/// resolved against the child's working directory. With neither `cwd` nor `env`
/// supplied there is nothing to correct, so the original resolution is used
/// unchanged.
#[cfg(windows)]
fn resolve_image(
    program: &str,
    cwd: Option<&Path>,
    env: &BTreeMap<String, Option<String>>,
) -> Result<std::ffi::OsString> {
    if cwd.is_none() && env.is_empty() {
        return refuse_windows_script(program);
    }

    let base = match cwd {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let path = effective_var("PATH", env).unwrap_or_default();
    let pathext =
        effective_var("PATHEXT", env).unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut extensions: Vec<String> = pathext
        .split(';')
        .map(str::trim)
        .filter(|extension| !extension.is_empty())
        .map(|extension| {
            if extension.starts_with('.') {
                extension.to_string()
            } else {
                format!(".{extension}")
            }
        })
        .collect();
    if extensions.is_empty() {
        extensions = vec![
            ".COM".to_string(),
            ".EXE".to_string(),
            ".BAT".to_string(),
            ".CMD".to_string(),
        ];
    }

    let has_separator = program.contains('\\') || program.contains('/');
    let has_extension = Path::new(program).extension().is_some();
    let mut roots: Vec<PathBuf> = Vec::new();
    if has_separator {
        let candidate = PathBuf::from(program);
        roots.push(if candidate.is_absolute() {
            candidate
        } else {
            base.join(candidate)
        });
    } else {
        // A bare name is looked for in the child's directory first, then along
        // the effective `PATH`.
        roots.push(base.join(program));
        for entry in path.split(';') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let dir = PathBuf::from(entry);
            let dir = if dir.is_absolute() {
                dir
            } else {
                base.join(dir)
            };
            roots.push(dir.join(program));
        }
    }

    for root in &roots {
        let candidates: Vec<PathBuf> = if has_extension {
            vec![root.clone()]
        } else {
            extensions
                .iter()
                .map(|extension| {
                    let mut text = root.clone().into_os_string();
                    text.push(extension);
                    PathBuf::from(text)
                })
                .collect()
        };
        for candidate in candidates {
            if candidate.is_file() {
                if script_extension(&candidate) {
                    return Err(script_refusal(program, &candidate));
                }
                return Ok(candidate.into_os_string());
            }
        }
    }

    // Nothing resolved. A literal command script is still refused as a script;
    // anything else is refused as missing rather than passed through, because
    // the caller named this directory and this environment, and letting
    // `CreateProcess` fall back to its own ambient search would start a program
    // those options did not resolve to.
    if script_extension(Path::new(program)) {
        return Err(script_refusal(program, Path::new(program)));
    }
    Err(safety::tool_missing(
        "proc_run",
        program,
        "it was not found in the requested working directory or the child's PATH",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Condvar;

    /// Every encoding a caller may name, so the pair below covers the whole
    /// option space rather than one mode's behaviour.
    const ENCODINGS: [Choice; 5] = [
        Choice::Utf8,
        Choice::Gbk,
        Choice::Utf16Le,
        Choice::Utf16Be,
        Choice::Bytes,
    ];

    fn bytes_of(outcome: &StreamOutcome) -> Vec<u8> {
        outcome.data.iter().copied().collect()
    }

    /// A capture no reader ever reported carries no text in any mode: reporting
    /// an empty string would be indistinguishable from an empty stream.
    #[test]
    fn a_capture_that_was_never_reported_offers_no_text() {
        for choice in ENCODINGS {
            let report = stream_report(choice, Capture::Prefix, None);
            assert_eq!(report.bytes, 0, "{}", choice.label());
            assert_eq!(report.total_bytes, 0, "{}", choice.label());
            assert_eq!(report.offset_bytes, 0, "{}", choice.label());
            assert_eq!(report.base64, "", "{}", choice.label());
            assert_eq!(report.text, None, "{}", choice.label());
            assert_eq!(report.decoded, None, "{}", choice.label());
            assert!(!report.complete, "{}", choice.label());
            assert!(!report.truncated, "{}", choice.label());
            assert!(report.log.is_none(), "{}", choice.label());
            let error = report.error.expect("a missing capture must say so");
            assert!(
                error.contains("none of its bytes are available"),
                "{}: {error}",
                choice.label()
            );
        }
    }

    /// The case above is not the empty stream: a reader that reported and read
    /// nothing produces empty text in a text mode and stays undecoded in
    /// `bytes` mode, with no error at all.
    #[test]
    fn a_reported_empty_stream_is_an_empty_stream() {
        let empty = StreamOutcome::default();
        for choice in ENCODINGS {
            let report = stream_report(choice, Capture::Prefix, Some(&empty));
            assert_eq!(report.bytes, 0, "{}", choice.label());
            assert_eq!(report.total_bytes, 0, "{}", choice.label());
            assert_eq!(report.base64, "", "{}", choice.label());
            assert!(report.complete, "{}", choice.label());
            assert_eq!(report.error, None, "{}", choice.label());
            match choice {
                Choice::Bytes => {
                    assert_eq!(report.text, None, "{}", choice.label());
                    assert_eq!(report.decoded, None, "{}", choice.label());
                }
                _ => {
                    assert_eq!(report.text.as_deref(), Some(""), "{}", choice.label());
                    assert_eq!(report.decoded, Some(true), "{}", choice.label());
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_scripts_are_refused_during_argument_validation() {
        for path in [
            r"C:\no-such-aether-test-dir\probe.cmd",
            r"C:\no-such-aether-test-dir\probe.BAT",
        ] {
            let error = match parse(vec![Value::Array(vec![Value::Str(path.into())])], None) {
                Err(error) => error,
                Ok(_) => panic!("a command script must be refused before the execution gate"),
            };
            assert!(error.to_string().contains("E_BAD_ARG"), "{error}");
            assert!(error.to_string().contains("command script"), "{error}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_argument_validation_pins_the_checked_image() {
        let executable = std::env::current_exe().expect("native test executable");
        let expected = which::which(&executable).expect("executable is resolvable");
        let request = parse(
            vec![Value::Array(vec![Value::Str(
                executable.to_string_lossy().into_owned(),
            )])],
            None,
        )
        .expect("native image is allowed");
        assert_eq!(request.image, expected.into_os_string());
    }

    /// A reader that delivers its bytes, announces that it is about to block,
    /// and then stays blocked until the test releases it.
    struct BlockingReader {
        head: Vec<u8>,
        sent: bool,
        reached: Arc<(Mutex<bool>, Condvar)>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Read for BlockingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if !self.sent {
                self.sent = true;
                let take = self.head.len().min(buffer.len());
                buffer[..take].copy_from_slice(&self.head[..take]);
                return Ok(take);
            }
            // `read_stream` stored the head before it called again, so
            // announcing here means the capture is readable once this is seen.
            signal(&self.reached);
            wait_until(&self.release, Duration::from_secs(30));
            Ok(0)
        }
    }

    /// Releases a blocked reader on every path, so a failing assertion cannot
    /// leave the reader waiting.
    struct ReleaseOnDrop(Arc<(Mutex<bool>, Condvar)>);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            signal(&self.0);
        }
    }

    fn signal(flag: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, condvar) = &**flag;
        *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        condvar.notify_all();
    }

    fn wait_until(flag: &Arc<(Mutex<bool>, Condvar)>, timeout: Duration) -> bool {
        let (lock, condvar) = &**flag;
        let mut signalled = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let deadline = Instant::now() + timeout;
        while !*signalled {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, _) = condvar
                .wait_timeout(signalled, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            signalled = next;
        }
        true
    }

    /// A capture that stopped at the caller's limit keeps the prefix it did
    /// read, and the report describes that prefix instead of the whole stream.
    #[test]
    fn a_truncated_capture_keeps_the_prefix_it_read() {
        let limit = AtomicBool::new(false);
        let log_stop = AtomicBool::new(false);
        let shared = Mutex::new(StreamOutcome::default());
        // Three GBK characters; only the first fits under the limit.
        let source = std::io::Cursor::new(vec![0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2]);

        read_stream(
            Box::new(source),
            ReaderPlan {
                index: 0,
                cap: 2,
                capture: Capture::Prefix,
                log: None,
            },
            &limit,
            &log_stop,
            &shared,
        );

        let captured = lock_capture(&shared).clone();
        assert_eq!(
            bytes_of(&captured),
            vec![0xD6, 0xD0],
            "the prefix must survive"
        );
        assert_eq!(captured.total_bytes, 6);
        assert!(captured.truncated);
        assert!(limit.load(Ordering::Relaxed));
        assert_eq!(captured.error, None);

        // GBK `D6 D0` is 中: the prefix decodes, and the report still says the
        // capture is partial rather than pretending the stream ended there.
        let report = stream_report(Choice::Gbk, Capture::Prefix, Some(&captured));
        assert_eq!(report.text.as_deref(), Some("中"));
        assert_eq!(report.decoded, Some(true));
        assert_eq!(report.bytes, 2);
        assert_eq!(report.total_bytes, 6);
        assert_eq!(report.offset_bytes, 0);
        assert_eq!(report.base64, "1tA=");
        assert!(!report.complete);
        assert!(report.truncated);
        let error = report.error.expect("a partial capture must say so");
        assert!(error.contains("only the first 2 bytes"), "{error}");
    }

    #[test]
    fn a_read_error_never_claims_a_complete_log() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _bytes: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected pipe failure"))
            }
        }
        let file = tempfile::tempfile().unwrap();
        let shared = Mutex::new(StreamOutcome {
            log: Some(LogOutcome::default()),
            ..Default::default()
        });
        read_stream(
            Box::new(Broken),
            ReaderPlan {
                index: 0,
                cap: 1024,
                capture: Capture::Tail,
                log: Some(LogSink {
                    file,
                    cap: 1024,
                    path: "private-log".into(),
                }),
            },
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            &shared,
        );
        let outcome = lock_capture(&shared);
        assert!(outcome.error.is_some());
        let log = outcome.log.as_ref().unwrap();
        assert!(!log.complete);
        assert!(log.error.as_ref().unwrap().contains("incomplete"));
    }

    /// The tail capture keeps the most recent bytes and says where they start.
    #[test]
    fn a_tail_capture_keeps_the_last_bytes_and_their_offset() {
        let limit = AtomicBool::new(false);
        let log_stop = AtomicBool::new(false);
        let shared = Mutex::new(StreamOutcome::default());
        let source = std::io::Cursor::new(b"abcdefghij".to_vec());

        read_stream(
            Box::new(source),
            ReaderPlan {
                index: 0,
                cap: 4,
                capture: Capture::Tail,
                log: None,
            },
            &limit,
            &log_stop,
            &shared,
        );

        let captured = lock_capture(&shared).clone();
        assert_eq!(bytes_of(&captured), b"ghij".to_vec());
        assert_eq!(captured.total_bytes, 10);
        assert!(captured.truncated);
        assert!(limit.load(Ordering::Relaxed));

        let report = stream_report(Choice::Utf8, Capture::Tail, Some(&captured));
        assert_eq!(report.capture, "tail");
        assert_eq!(report.bytes, 4);
        assert_eq!(report.total_bytes, 10);
        assert_eq!(report.offset_bytes, 6);
        assert_eq!(report.text.as_deref(), Some("ghij"));
        assert!(!report.complete);
        let error = report.error.expect("a partial tail must say so");
        assert!(error.contains("only the last 4 bytes"), "{error}");
    }

    /// A stream shorter than the limit is complete in either mode.
    #[test]
    fn a_short_tail_is_a_whole_capture() {
        let limit = AtomicBool::new(false);
        let log_stop = AtomicBool::new(false);
        let shared = Mutex::new(StreamOutcome::default());
        let source = std::io::Cursor::new(b"abc".to_vec());

        read_stream(
            Box::new(source),
            ReaderPlan {
                index: 0,
                cap: 16,
                capture: Capture::Tail,
                log: None,
            },
            &limit,
            &log_stop,
            &shared,
        );

        let captured = lock_capture(&shared).clone();
        assert_eq!(bytes_of(&captured), b"abc".to_vec());
        assert_eq!(captured.total_bytes, 3);
        assert!(!captured.truncated);
        assert!(!limit.load(Ordering::Relaxed));
        let report = stream_report(Choice::Utf8, Capture::Tail, Some(&captured));
        assert!(report.complete);
        assert_eq!(report.offset_bytes, 0);
        assert_eq!(report.error, None);
    }

    /// The core of the snapshot: a reader that never reaches the end of the
    /// stream has still stored the bytes it read, and the caller can read them
    /// while the reader is blocked. No sleep decides anything here — the reader
    /// itself announces when it is about to block.
    #[test]
    fn a_blocked_reader_leaves_its_prefix_where_the_caller_can_read_it() {
        let head = vec![0xD6, 0xD0];
        let reached = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let _released_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let reader = BlockingReader {
            head: head.clone(),
            sent: false,
            reached: Arc::clone(&reached),
            release: Arc::clone(&release),
        };

        let limit = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let (handle, shared) = spawn_reader(
            Box::new(reader),
            ReaderPlan {
                index: 0,
                cap: 1024,
                capture: Capture::Prefix,
                log: None,
            },
            Arc::clone(&limit),
            Arc::new(AtomicBool::new(false)),
            sender,
        )
        .expect("spawn the reader");

        assert!(
            wait_until(&reached, Duration::from_secs(30)),
            "the reader never reached its blocking read"
        );
        assert!(
            receiver.try_recv().is_err(),
            "a blocked reader must not have reported anything"
        );

        let captured = lock_capture(&shared).clone();
        assert_eq!(
            bytes_of(&captured),
            head,
            "the prefix must be readable while the reader is still blocked"
        );
        assert!(!captured.truncated);
        assert_eq!(captured.error, None);

        // Release before asserting anything else, so the reader is never left
        // waiting on a path that panics.
        signal(&release);
        let reported = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("a released reader must report");
        let _ = handle.join();
        assert_eq!(reported.0, 0);
        assert_eq!(bytes_of(&reported.1), head);
        assert_eq!(reported.1.error, None);
    }

    #[test]
    fn every_option_is_validated_before_anything_is_resolved() {
        let program = Value::Array(vec![Value::Str("probe".into())]);
        let record = |pairs: &[(&str, Value)]| {
            Value::Record(
                pairs
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), value.clone()))
                    .collect(),
            )
        };

        let env_record = |pairs: &[(&str, &str)]| {
            Value::Record(
                pairs
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), Value::Str((*value).to_string())))
                    .collect(),
            )
        };
        let managed = |name: &str| record(&[("env", env_record(&[(name, "x")]))]);

        for (options, why) in [
            (
                record(&[("capture", Value::Str("middle".into()))]),
                "unknown capture",
            ),
            (
                record(&[("on_output_limit", Value::Str("ignore".into()))]),
                "unknown on_output_limit",
            ),
            (
                record(&[("capture", Value::Int(1))]),
                "capture is not a String",
            ),
            (
                record(&[("on_output_limit", Value::Null)]),
                "on_output_limit is not a String",
            ),
            (record(&[("cwd", Value::Str(String::new()))]), "empty cwd"),
            (record(&[("cwd", Value::Str("a\0b".into()))]), "NUL in cwd"),
            (record(&[("cwd", Value::Int(2))]), "cwd is not a String"),
            (
                record(&[("max_log_bytes", Value::Int(0))]),
                "zero log limit",
            ),
            (
                record(&[("max_log_bytes", Value::Int(1_073_741_825))]),
                "log limit past the cap",
            ),
            (
                record(&[("stdout_log", Value::Str(String::new()))]),
                "empty log path",
            ),
            (
                record(&[("stderr_log", Value::Int(3))]),
                "log path is not a String",
            ),
            (
                record(&[
                    ("stdin", Value::Str("a".into())),
                    ("stdin_base64", Value::Str("YQ==".into())),
                ]),
                "stdin given twice",
            ),
            (
                record(&[("stdin_base64", Value::Str("not base64!".into()))]),
                "stdin_base64 that is not base64",
            ),
            (record(&[("stdin", Value::Int(4))]), "stdin is not a String"),
            (
                record(&[("env", Value::Array(vec![]))]),
                "env is not a Record",
            ),
            (managed("AETHER_MODE"), "a managed environment name"),
            (managed("DSH_TOKEN"), "a managed environment name"),
            (managed("AE_POLICY"), "a managed environment name"),
            (
                record(&[("env", env_record(&[("GOOD", "a\0b")]))]),
                "a NUL in an environment value",
            ),
            (
                record(&[("env", env_record(&[("A=B", "x")]))]),
                "an environment name with '='",
            ),
        ] {
            let error = match parse(vec![program.clone(), options], None) {
                Err(error) => error,
                Ok(_) => panic!("{why} must be refused"),
            };
            assert!(error.to_string().contains("E_BAD_ARG"), "{why}: {error}");
        }
    }

    /// The last option the caller may name is accepted, and the defaults are
    /// what the module documents.
    #[test]
    fn the_public_option_defaults_are_the_documented_ones() {
        let options = Options::default();
        assert_eq!(options.capture, Capture::Prefix);
        assert_eq!(options.on_limit, OnLimit::Terminate);
        assert_eq!(options.max_output, 1_048_576);
        assert_eq!(options.max_log, 67_108_864);
        assert!(options.cwd.is_none());
        assert!(options.env.is_empty());
        assert!(options.stdin.is_none());
        assert_eq!(options.logs, [None, None]);

        let request = parse(
            vec![
                Value::Array(vec![Value::Str("probe".into())]),
                Value::Record(BTreeMap::from([
                    ("capture".to_string(), Value::Str("tail".into())),
                    ("on_output_limit".to_string(), Value::Str("continue".into())),
                    ("max_log_bytes".to_string(), Value::Int(1_073_741_824)),
                ])),
            ],
            None,
        )
        .expect("the largest legal bounds are accepted");
        assert_eq!(request.capture, Capture::Tail);
        assert_eq!(request.on_limit, OnLimit::Continue);
        assert_eq!(request.max_log, 1_073_741_824);
    }

    /// Environment values never reach a diagnostic, and a refusal names the
    /// option it refused rather than anything it was given.
    #[test]
    fn environment_values_stay_out_of_refusals() {
        let secret = "s3cr3t-value-that-must-not-be-echoed";
        let error = match parse(
            vec![
                Value::Array(vec![Value::Str("probe".into())]),
                Value::Record(BTreeMap::from([
                    (
                        "env".to_string(),
                        Value::Record(BTreeMap::from([(
                            "FORK_PROC_SECRET".to_string(),
                            Value::Str(secret.into()),
                        )])),
                    ),
                    ("stdout_log".to_string(), Value::Str(String::new())),
                ])),
            ],
            None,
        ) {
            Err(error) => error,
            Ok(_) => panic!("an empty log path must be refused"),
        };
        assert!(error.to_string().contains("E_BAD_ARG"), "{error}");
        assert!(!error.to_string().contains(secret), "{error}");
    }

    /// An absent `stdin` is not a failure: nothing was requested and there is
    /// nothing left to write, so it is initialized complete. A requested stdin
    /// that was cut short is not, and that field is what the top-level verdict
    /// reads without touching the child's own `process.success`.
    #[test]
    fn an_absent_stdin_is_complete_and_a_short_one_is_not() {
        let absent = StdinOutcome::default();
        assert!(!absent.requested);
        assert_eq!(absent.bytes, 0);
        assert_eq!(absent.written_bytes, 0);
        assert!(absent.complete);
        assert_eq!(absent.error, None);

        let short = StdinOutcome {
            requested: true,
            bytes: 10,
            written_bytes: 4,
            complete: false,
            error: Some("the child closed stdin before every byte was written".to_string()),
        };
        assert!(!short.complete);
        let record = match stdin_value(&short) {
            Value::Record(record) => record,
            other => panic!("stdin_value builds a record, not {other:?}"),
        };
        let flag = |key: &str| match record.get(key) {
            Some(Value::Bool(value)) => *value,
            other => panic!("`{key}` is {other:?}, not a Bool"),
        };
        let number = |key: &str| match record.get(key) {
            Some(Value::Int(value)) => *value,
            other => panic!("`{key}` is {other:?}, not an Int"),
        };
        assert!(flag("requested"));
        assert!(!flag("complete"));
        assert_eq!(number("bytes"), 10);
        assert_eq!(number("written_bytes"), 4);
    }
}

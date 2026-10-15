//! `proc_run` — run one program from an explicit argv and report the process
//! and the decoding of its streams as separate facts.
//!
//! The caller writes the program and every argument literally: no shell parses
//! them, so `|`, `*`, `$(...)` and quotes inside an argument are data. The
//! working directory and environment are inherited from the shell, stdin is
//! disconnected, and the child becomes the leader of its own managed group — a
//! POSIX process group, or a Windows job object when this is a Windows build.
//!
//! # Result
//!
//! ```text
//! { success: Bool
//! , process: { success: Bool, exit_code: Int|Null, timed_out: Bool
//!            , output_limit_exceeded: Bool, cleanup_complete: Bool
//!            , group_termination_requested: Bool }
//! , stdout: Stream
//! , stderr: Stream }
//!
//! Stream = { encoding: String, bytes: Int, base64: String, text: String|Null
//!          , decoded: Bool|Null, complete: Bool, error: String|Null }
//! ```
//!
//! `bytes` counts the bytes that were *captured*, which is the whole stream
//! unless `complete` is false — `max_output_bytes` bounds each stream
//! separately and the total the child produced is never measured. `base64`
//! always holds exactly those captured bytes, so it stays meaningful when
//! decoding fails. A stream whose reader is still blocked on a pipe the process
//! holds keeps the prefix it has already read: `bytes`, `base64` and `text`
//! describe that prefix, `complete` is false, and an `error` says the capture is
//! still open. Only a stream with no reader at all leaves `text` and `decoded`
//! null, because no bytes ever arrived.
//!
//! `process.success` means only "the leader exited normally with code 0".
//! `group_termination_requested` says the request to terminate the managed group
//! was accepted, or that the group was already gone: it is an acknowledgement,
//! not evidence that every process in the group has exited. `cleanup_complete`
//! is deliberately narrower and about this call's own resources — the leader was
//! reaped and both capture readers were joined, so no pipe or thread started
//! here is left behind.
//!
//! The top-level `success` adds: no timeout, no discarded output, termination
//! requested, cleanup complete, and every stream decoded. A nonzero exit, a
//! truncated capture and a failed decode are reported, never raised — the raw
//! bytes and the exit status survive all three.
//!
//! # Decoding
//!
//! `encoding` names one codec exactly: no charset sniffing, no code page, no
//! replacement character. Bytes that are not valid in the named encoding give
//! `text: null`, `decoded: false` and an actionable `error`, with the bytes
//! intact in `base64`. A byte-order mark is **not** removed, because the caller
//! chose the encoding and the mark is part of the stream: UTF-16 input starting
//! `FF FE` decodes to U+FEFF followed by its characters. Newlines are never
//! rewritten.
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
//! operating system and `cleanup_complete` says the leader was reaped and both
//! capture readers were joined; neither proves that no descendant survives, and
//! no non-blocking check available here can prove it. Nothing in this call runs
//! if the interpreter itself is killed outright — the same reason in-process
//! cleanup cannot catch `SIGKILL` on POSIX — so descendants of an abruptly
//! terminated interpreter are outside this contract.
//!
//! This is process-group ownership, not a sandbox: the effect policy is the
//! control on what may be started.
//!
//! On Windows a `.cmd` or `.bat` program — including a bare name that `PATH`
//! resolves to one — is refused before anything starts, because running it means
//! `cmd.exe` interprets it, which is the implicit shell this builtin does not
//! have. Name the interpreter explicitly (`["cmd.exe", "/c", ...]`) to run one
//! on purpose.

use std::collections::BTreeMap;
use std::io::Read;
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

use crate::safety::{self, ErrorCode, SafetyError};
use crate::value::Value;

const DEFAULT_TIMEOUT_MS: i64 = 120_000;
const MAX_TIMEOUT_MS: i64 = 600_000;
const DEFAULT_MAX_OUTPUT_BYTES: i64 = 1_048_576;
const MAX_OUTPUT_BYTES: i64 = 16_777_216;

/// How long a killed managed group gets to close its pipes and be reaped before
/// the call stops waiting for it.
const CLEANUP_GRACE: Duration = Duration::from_secs(5);
/// How long the wait loop sleeps before it re-reads the child and the captures.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// One read per syscall: 16 MiB of captured output is 256 reads.
const READ_CHUNK: usize = 65_536;

const ENCODING_EXPECTED: &str =
    "encoding to be one of \"utf-8\", \"gbk\", \"utf-16le\", \"utf-16be\" or \"bytes\"";
const STREAMS: [&str; 2] = ["stdout", "stderr"];

/// Run `proc_run(argv, options?)` and answer its result record.
///
/// `input` is refused: the argv is the call, and silently accepting a piped
/// subject would make `x | proc_run(...)` mean something an agent cannot see.
pub fn run(args: Vec<Value>, input: Option<Value>) -> Result<Value> {
    let request = parse(args, input)?;

    // `proc_run` is classified `Effect::Exec` and listed in
    // `safety::SELF_GUARDED`, so `guard_dispatch` deliberately does not gate it;
    // this call is the gate. It runs after argument validation (a refused call
    // must not prompt) and before the process exists, and every route that
    // reaches this function — the dispatcher, `proc.run`, a direct call — passes
    // through it.
    safety::guard_exec("proc_run", audit_command(&request.argv))?;

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
}

/// One of the five encodings the caller may name.
#[derive(Clone, Copy, PartialEq, Eq)]
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

fn parse(args: Vec<Value>, input: Option<Value>) -> Result<Request> {
    if let Some(piped) = input {
        return Err(safety::bad_arg(
            "proc_run",
            "no piped input: the argv is the first argument",
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
    let (encoding, timeout, max_output) = match args.next() {
        Some(Value::Record(options)) => parse_options(&options)?,
        Some(other) => {
            return Err(safety::bad_arg(
                "proc_run",
                "options as a Record with only encoding, timeout_ms and max_output_bytes",
                other.type_name(),
            ))
        }
        None => (
            Choice::Utf8,
            Duration::from_millis(DEFAULT_TIMEOUT_MS as u64),
            DEFAULT_MAX_OUTPUT_BYTES as usize,
        ),
    };
    if let Some(extra) = args.next() {
        return Err(safety::bad_arg(
            "proc_run",
            "at most two arguments: argv and options",
            &format!("a third argument of type {}", extra.type_name()),
        ));
    }

    // Resolved here, before the effect gate: a call that is going to be refused
    // — an unapproved program, or a Windows command script — must not prompt
    // first, and the path that is checked has to be the path that is started.
    #[cfg(windows)]
    let image = refuse_windows_script(&argv[0])?;

    Ok(Request {
        argv,
        #[cfg(windows)]
        image,
        encoding,
        timeout,
        max_output,
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

fn parse_options(options: &BTreeMap<String, Value>) -> Result<(Choice, Duration, usize)> {
    for key in options.keys() {
        if !matches!(key.as_str(), "encoding" | "timeout_ms" | "max_output_bytes") {
            return Err(safety::bad_arg(
                "proc_run",
                "an options Record whose only keys are encoding, timeout_ms and max_output_bytes",
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

    Ok((
        encoding,
        Duration::from_millis(timeout as u64),
        max_output as usize,
    ))
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

/// The command string the exec audit entry records: every argument rendered with
/// Rust's string escaping, so an argument containing a space, a quote or a
/// newline cannot read as a second argument or a second line. Bounded, so an
/// argv of any size cannot flood the log.
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

/// The bytes one reader thread captured, and how the capture ended.
///
/// The reader updates this under a shared lock as it reads, so the prefix it has
/// already read is available even if it never reaches the end of the stream.
#[derive(Clone, Default)]
struct StreamOutcome {
    data: Vec<u8>,
    /// The stream was longer than the caller's limit.
    truncated: bool,
    /// The read itself failed; the bytes above are what arrived before that.
    error: Option<String>,
}

/// The capture a reader builds, shared with the caller.
type SharedCapture = Arc<Mutex<StreamOutcome>>;

fn lock_capture(shared: &Mutex<StreamOutcome>) -> MutexGuard<'_, StreamOutcome> {
    // A panic in a reader must not turn a partial capture into an error about
    // the lock; the bytes already stored are still the answer.
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

fn execute(request: Request) -> Result<Value> {
    let program = request.argv[0].clone();
    // `image` is the path validation resolved and checked on Windows; elsewhere
    // the program name is passed through unchanged.
    #[cfg(windows)]
    let image = request.image.clone();
    #[cfg(not(windows))]
    let image: std::ffi::OsString = program.clone().into();

    let mut wrap = CommandWrap::with_new(&image, |command| {
        command
            .args(&request.argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    });
    // The managed group: a POSIX process group on Unix, a job object on Windows.
    #[cfg(unix)]
    wrap.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    wrap.wrap(JobObject);
    let mut child = wrap
        .spawn()
        .map_err(|error| safety::spawn_error("proc_run", &program, &error))?;
    // A spawn failure is reported as a structured error and nothing is waited
    // for: there is no handle for a process that could not be started, so any
    // cleanup of a half-created group belongs to the spawn implementation.

    // Both pipes are taken before anything waits on the child: reading stdout to
    // end before touching stderr deadlocks a child that fills stderr first, and
    // a reader thread per stream avoids that shape entirely.
    let stdout_pipe = child
        .stdout()
        .take()
        .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
    let stderr_pipe = child
        .stderr()
        .take()
        .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
    let sources: [Option<Box<dyn Read + Send>>; 2] = [stdout_pipe, stderr_pipe];

    let mut guard = GroupGuard::new(child);

    let limit = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel::<(usize, StreamOutcome)>();
    let mut handles: [Option<JoinHandle<()>>; 2] = [None, None];
    let mut captures: [Option<SharedCapture>; 2] = [None, None];
    let mut outcomes: [Option<StreamOutcome>; 2] = [None, None];
    let mut root: Option<ExitStatus> = None;

    for (index, source) in sources.into_iter().enumerate() {
        let Some(source) = source else { continue };
        let spawned = spawn_reader(
            source,
            index,
            request.max_output,
            Arc::clone(&limit),
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
                    Instant::now() + CLEANUP_GRACE,
                );
                guard.disarm();
                return Err(reader_error(&program, &error));
            }
        }
    }
    drop(sender);

    let deadline = Instant::now() + request.timeout;
    let mut timed_out = false;
    let mut stop_at: Option<Instant> = None;

    loop {
        drain(&receiver, &mut outcomes);
        join_finished(&mut handles, &mut outcomes, &receiver);
        if root.is_none() {
            root = guard.try_wait();
        }
        // A leader that exited is not the end of the call: the captures must be
        // closed too, or a descendant is still writing into them.
        if root.is_some() && captured(&outcomes) {
            break;
        }

        let now = Instant::now();
        if let Some(end) = stop_at {
            if now >= end {
                break;
            }
        } else if limit.load(Ordering::Relaxed) {
            // The caller asked for at most this much, and the reader is already
            // discarding the rest; killing the group stops the writer.
            stop_at = Some(now + CLEANUP_GRACE);
            guard.kill_group();
        } else if now >= deadline {
            timed_out = true;
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
        stop_at.unwrap_or_else(|| Instant::now() + CLEANUP_GRACE),
    );
    guard.disarm();

    // One last look at the readers that finished while the bound ran out: a
    // joined reader has already queued its result, so this is what tells a
    // completed capture apart from one that is still blocked.
    join_finished(&mut handles, &mut outcomes, &receiver);

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
                 counts the prefix read so far and `complete` is false"
                    .to_string(),
            );
        }
        outcomes[index] = Some(partial);
    }

    let stdout = stream_report(request.encoding, outcomes[0].as_ref());
    let stderr = stream_report(request.encoding, outcomes[1].as_ref());
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
    // Narrow on purpose: the leader was reaped and every reader this call
    // started was joined. Handles are `None` only after `join`, so a reader that
    // had to be detached — a descendant outside the managed group can hold a
    // pipe open — leaves this false and an incomplete capture behind.
    let cleanup_complete = root.is_some() && handles[0].is_none() && handles[1].is_none();
    let success = process_success
        && !timed_out
        && !output_limit_exceeded
        && termination_requested
        && cleanup_complete
        && stdout.complete
        && stderr.complete
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
    record.insert("stdout".to_string(), stdout.to_value());
    record.insert("stderr".to_string(), stderr.to_value());
    Ok(Value::Record(record))
}

/// Read one stream into a bounded buffer, then keep draining to end of stream.
///
/// Draining matters: stopping at the limit would leave the child blocked on a
/// full pipe and turn a bounded capture into a hang. What is discarded is
/// counted by `truncated`, which is the honest statement that the capture is
/// partial — the total the child produced is not measured.
///
/// Every chunk is stored in `shared` as it is read, for two reasons: the prefix
/// survives a reader that never reaches the end of the stream, and the caller
/// can read it at any time without waiting for this thread to finish.
fn read_bounded(
    mut source: Box<dyn Read + Send>,
    cap: usize,
    limit: &AtomicBool,
    shared: &Mutex<StreamOutcome>,
) {
    let mut buffer = vec![0u8; READ_CHUNK];

    loop {
        match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let mut captured = lock_capture(shared);
                let room = cap - captured.data.len();
                if read <= room {
                    captured.data.extend_from_slice(&buffer[..read]);
                } else {
                    captured.data.extend_from_slice(&buffer[..room]);
                    captured.truncated = true;
                    limit.store(true, Ordering::Relaxed);
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
}

fn spawn_reader(
    source: Box<dyn Read + Send>,
    index: usize,
    cap: usize,
    limit: Arc<AtomicBool>,
    sender: Sender<(usize, StreamOutcome)>,
) -> std::io::Result<(JoinHandle<()>, SharedCapture)> {
    let shared: SharedCapture = Arc::new(Mutex::new(StreamOutcome::default()));
    let reader_state = Arc::clone(&shared);
    let handle = thread::Builder::new()
        .name(format!("proc_run.{}", STREAMS[index]))
        .spawn(move || {
            read_bounded(source, cap, &limit, &reader_state);
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

fn reader_error(program: &str, error: &std::io::Error) -> anyhow::Error {
    anyhow::Error::new(SafetyError {
        code: ErrorCode::Io,
        message: format!("proc_run: cannot start an output reader thread for `{program}`: {error}"),
        builtin: "proc_run".to_string(),
        hint: "the host refused a capture thread; group termination and bounded cleanup were \
               requested, and no output is returned for this infrastructure failure"
            .to_string(),
        approval: None,
        did_you_mean: Vec::new(),
        expected: "one reader thread per stream".to_string(),
        got: error.kind().to_string(),
    })
}

/// Wait, bounded, for the captures to close and the leader to be reaped; then
/// join the readers that finished and detach the ones that did not.
///
/// Detaching is the point of the bound: a descendant that left the managed group
/// can hold a pipe open indefinitely, and a detached reader cannot block the
/// caller or keep the process alive. The stream it was reading is reported
/// incomplete instead, which is what happened. Such a reader is the one resource
/// this call can leave behind, and only when a pipe outlived the bound; a reader
/// that cannot be started at all is reported as a coded error rather than as a
/// cleanup flag.
fn settle(
    guard: &mut GroupGuard,
    handles: &mut [Option<JoinHandle<()>>; 2],
    outcomes: &mut [Option<StreamOutcome>; 2],
    receiver: &Receiver<(usize, StreamOutcome)>,
    root: &mut Option<ExitStatus>,
    end: Instant,
) {
    loop {
        drain(receiver, outcomes);
        join_finished(handles, outcomes, receiver);
        if root.is_none() {
            *root = guard.try_wait();
        }
        if captured(outcomes) && root.is_some() {
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
}

fn drain(receiver: &Receiver<(usize, StreamOutcome)>, outcomes: &mut [Option<StreamOutcome>; 2]) {
    while let Ok((index, outcome)) = receiver.try_recv() {
        outcomes[index] = Some(outcome);
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
                data: Vec::new(),
                truncated: false,
                error: Some("the output reader ended without reporting what it read".to_string()),
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
    bytes: usize,
    base64: String,
    text: Option<String>,
    decoded: Option<bool>,
    complete: bool,
    truncated: bool,
    error: Option<String>,
}

impl StreamReport {
    fn to_value(&self) -> Value {
        let mut record = BTreeMap::new();
        record.insert(
            "encoding".to_string(),
            Value::Str(self.encoding.to_string()),
        );
        record.insert("bytes".to_string(), Value::Int(self.bytes as i64));
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
        Value::Record(record)
    }
}

fn stream_report(choice: Choice, outcome: Option<&StreamOutcome>) -> StreamReport {
    // A reader that never reported leaves nothing to decode. Answering with an
    // empty text would read as a stream that really was empty, which is why
    // `text` and `decoded` are null here rather than `""` and `true`.
    let Some(outcome) = outcome else {
        return StreamReport {
            encoding: choice.label(),
            bytes: 0,
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
        };
    };

    let data: &[u8] = &outcome.data;
    let (text, decoded, decode_error) = match choice.codec() {
        None => (None, None, None),
        Some(codec) => match codec.decode_without_bom_handling_and_without_replacement(data) {
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
            outcome.truncated.then(|| {
                format!(
                    "only the first {} bytes were captured: the stream was longer, so the \
                     rest was discarded",
                    data.len()
                )
            })
        })
        .or(decode_error);

    StreamReport {
        encoding: choice.label(),
        bytes: data.len(),
        base64: {
            use base64::{engine::general_purpose::STANDARD, Engine as _};
            STANDARD.encode(data)
        },
        text,
        decoded,
        complete: !outcome.truncated && outcome.error.is_none(),
        truncated: outcome.truncated,
        error,
    }
}

// ── Windows: no implicit command interpreter ────────────────────────────

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
    use std::path::{Path, PathBuf};

    let resolved = which::which(program).ok();
    let candidate: &Path = resolved.as_deref().unwrap_or_else(|| Path::new(program));
    let script = candidate
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        });

    if script {
        return Err(anyhow::Error::new(SafetyError {
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
        }));
    }

    Ok(resolved
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from(program)))
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

    /// A capture no reader ever reported carries no text in any mode: reporting
    /// an empty string would be indistinguishable from an empty stream.
    #[test]
    fn a_capture_that_was_never_reported_offers_no_text() {
        for choice in ENCODINGS {
            let report = stream_report(choice, None);
            assert_eq!(report.bytes, 0, "{}", choice.label());
            assert_eq!(report.base64, "", "{}", choice.label());
            assert_eq!(report.text, None, "{}", choice.label());
            assert_eq!(report.decoded, None, "{}", choice.label());
            assert!(!report.complete, "{}", choice.label());
            assert!(!report.truncated, "{}", choice.label());
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
        let empty = StreamOutcome {
            data: Vec::new(),
            truncated: false,
            error: None,
        };
        for choice in ENCODINGS {
            let report = stream_report(choice, Some(&empty));
            assert_eq!(report.bytes, 0, "{}", choice.label());
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
            // `read_bounded` stored the head before it called again, so
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
        let shared = Mutex::new(StreamOutcome::default());
        // Three GBK characters; only the first fits under the limit.
        let source = std::io::Cursor::new(vec![0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2]);

        read_bounded(Box::new(source), 2, &limit, &shared);

        let captured = lock_capture(&shared).clone();
        assert_eq!(captured.data, vec![0xD6, 0xD0], "the prefix must survive");
        assert!(captured.truncated);
        assert!(limit.load(Ordering::Relaxed));
        assert_eq!(captured.error, None);

        // GBK `D6 D0` is 中: the prefix decodes, and the report still says the
        // capture is partial rather than pretending the stream ended there.
        let report = stream_report(Choice::Gbk, Some(&captured));
        assert_eq!(report.text.as_deref(), Some("中"));
        assert_eq!(report.decoded, Some(true));
        assert_eq!(report.bytes, 2);
        assert_eq!(report.base64, "1tA=");
        assert!(!report.complete);
        assert!(report.truncated);
        let error = report.error.expect("a partial capture must say so");
        assert!(error.contains("only the first 2 bytes"), "{error}");
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
        let (handle, shared) = spawn_reader(Box::new(reader), 0, 1024, Arc::clone(&limit), sender)
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
            captured.data, head,
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
        assert_eq!(reported.1.data, head);
        assert_eq!(reported.1.error, None);
    }
}

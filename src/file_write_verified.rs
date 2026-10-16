//! `file_write_verified` — write one `String`, publish it atomically, then read
//! the target back and report whether the bytes are the ones written.
//!
//! Local contract, in the order it is enforced: every argument and option is
//! typed and checked, the content is encoded with the codec the caller named
//! (never guessed, never substituted), the resolved target is validated and
//! guarded, the encoded bytes go to a temporary file in the target's own
//! directory and are renamed onto it, and the target is reopened and compared
//! against those bytes with a bounded read.
//!
//! `written` is what the publish returned. `verified` is what the read-back
//! observed at that moment — bytes that differ from the ones written give
//! `success: false` and `verified: false`, and nothing after the comparison is
//! covered by it. Verification never undoes a write, including a write this call
//! did not make.
//!
//! `expected_bytes` and `expected_sha256` describe the bytes this call wrote;
//! `precondition_sha256` echoes the caller's `expected_sha256` option, which is
//! the digest the target had to carry *before* the write. The path is inspected
//! as named — an existing symbolic link or other non-regular file is refused
//! rather than followed — and containment is the security module's path policy.
//!
//! The result fields, the option set and the durability limits are documented
//! with the builtin's signature; this module states only what it orders.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::safety::{self, Effect, GuardCtx};
use crate::security;
use crate::value::Value;

/// The name every refusal, audit entry and guard target carries.
const BUILTIN: &str = "file_write_verified";
/// The verification method reported in `verification.method`.
const VERIFICATION_METHOD: &str = "readback-bytes-sha256";
/// Temporary files are created beside the target with a name that cannot be
/// mistaken for one of the caller's own.
const TEMP_PREFIX: &str = ".aether-verified-";
const DEFAULT_MAX_BYTES: i64 = 1_048_576;
const MAX_MAX_BYTES: i64 = 16_777_216;
const OPTION_KEYS: [&str; 5] = [
    "encoding",
    "bom",
    "max_bytes",
    "expected_sha256",
    "create_only",
];
const ENCODING_EXPECTED: &str =
    "encoding to be one of \"utf-8\", \"gbk\", \"utf-16le\" or \"utf-16be\"";
const BOM_EXPECTED: &str = "bom to be \"none\" or \"add\"";
const MAX_BYTES_EXPECTED: &str = "max_bytes to be an Int from 1 to 16777216";
const SHA256_EXPECTED: &str = "expected_sha256 to be a 64-character hexadecimal string";

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
const UTF16LE_BOM: &[u8] = b"\xFF\xFE";
const UTF16BE_BOM: &[u8] = b"\xFE\xFF";

/// Write `file_write_verified(path, content, options?)` and answer its record.
///
/// `input` is refused rather than ignored: the content is the call's second
/// argument, and a piped document would otherwise be written without ever being
/// named in the call.
///
/// # Errors
///
/// Every refusal is a coded `safety::SafetyError`. A malformed call, an
/// unmappable character, an oversized document, a target that is a link or not a
/// regular file, a path the security module refuses, and a failed
/// `expected_sha256` precondition all fail before anything is written. A failure
/// to publish the temporary file fails too. A failure of the *read-back* does
/// not: the write happened, so it comes back as a record with `success: false`.
pub fn run(args: Vec<Value>, input: Option<Value>) -> Result<Value> {
    let request = parse(args, input)?;

    // Encoding is pure, and it decides whether the call can happen at all: an
    // unmappable character or an oversized document is refused before the effect
    // gate (a refused call must not prompt or audit) and before any filesystem
    // call.
    let encoded = encode(&request)?;

    let resolved = safety::resolve_path_str(&request.path);
    // The caller's `max_bytes` bounded the encoder; this adds the operator's
    // shell-wide policy, and it is settled here — before the effect gate, so a
    // refused call neither prompts nor audits, and before a temporary file
    // exists.
    enforce_size(
        encoded.len() as u64,
        request.max_bytes,
        &resolved,
        "to write",
    )?;
    let target = validate_target(&resolved)?;

    // `file_write_verified` is in `safety::SELF_GUARDED`, so `guard_dispatch`
    // does not gate it. This is the gate. It lives inside `run` rather than at
    // the dispatcher because `file_write_verified::run` is public and callable
    // on its own, and a direct caller must pass through it too.
    safety::guard(GuardCtx {
        builtin: BUILTIN,
        effect: Effect::WriteLocal,
        what: "write",
        targets: vec![resolved.clone()],
        blast_radius: serde_json::json!({ "bytes": encoded.len() }),
        reversible: true,
        fs_paths: true,
    })?;

    write_then_verify(&target, &resolved, &encoded, &request.spec(), read_bounded)
}

// ── arguments ───────────────────────────────────────────────────────────

/// The validated call: everything the commit step needs, resolved and bounded.
struct Request {
    path: String,
    content: String,
    encoding: Choice,
    bom: Bom,
    /// The byte envelope for the encoded document, BOM included. It bounds both
    /// the write and the bounded read `expected_sha256` and the read-back use.
    max_bytes: usize,
    /// The caller's `expected_sha256`, normalized to lowercase hex.
    expected_sha256: Option<String>,
    create_only: bool,
}

impl Request {
    fn spec(&self) -> Spec {
        Spec {
            encoding: self.encoding,
            bom: self.bom,
            max_bytes: self.max_bytes,
            create_only: self.create_only,
            precondition_sha256: self.expected_sha256.clone(),
        }
    }
}

/// The parts of a validated call that [`write_then_verify`] acts on.
struct Spec {
    encoding: Choice,
    bom: Bom,
    max_bytes: usize,
    create_only: bool,
    precondition_sha256: Option<String>,
}

/// One of the four codecs the caller may name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Utf8,
    Gbk,
    Utf16Le,
    Utf16Be,
}

impl Choice {
    /// The exact spelling, or nothing — the four labels the write contract
    /// documents and `proc_run`/`file_read_text` accept. No alias and no case
    /// folding: an unknown label is a refusal, never a guess at what the caller
    /// meant.
    fn parse(label: &str) -> Option<Self> {
        Some(match label {
            "utf-8" => Self::Utf8,
            "gbk" => Self::Gbk,
            "utf-16le" => Self::Utf16Le,
            "utf-16be" => Self::Utf16Be,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Gbk => "gbk",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
        }
    }
}

/// Whether the matching byte-order mark is prepended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bom {
    None,
    Add,
}

impl Bom {
    /// The exact spelling, or nothing — `none` (the default) and `add` are the
    /// two documented values, and an unknown label is a refusal rather than a
    /// guess at what the caller meant.
    fn parse(label: &str) -> Option<Self> {
        Some(match label {
            "none" => Self::None,
            "add" => Self::Add,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Add => "add",
        }
    }

    /// The prefix this mode adds under `encoding`; `Ok(&[])` is no mark at all.
    ///
    /// GBK is an `Err` rather than an empty slice: `parse` refuses that
    /// combination, and repeating the refusal here means the encoder cannot
    /// silently write an unmarked GBK document if that check ever moves.
    fn mark(self, encoding: Choice) -> Result<&'static [u8]> {
        Ok(match self {
            Self::None => &[],
            Self::Add => match encoding {
                Choice::Utf8 => UTF8_BOM,
                Choice::Utf16Le => UTF16LE_BOM,
                Choice::Utf16Be => UTF16BE_BOM,
                Choice::Gbk => return Err(gbk_bom_refusal()),
            },
        })
    }
}

fn parse(args: Vec<Value>, input: Option<Value>) -> Result<Request> {
    if let Some(piped) = input {
        return Err(safety::bad_arg(
            BUILTIN,
            "no piped input: the content is the second argument",
            piped.type_name(),
        ));
    }

    let mut args = args.into_iter();
    let path = match args.next() {
        Some(Value::Str(path)) => path,
        Some(other) => return Err(safety::bad_arg(BUILTIN, "path: String", other.type_name())),
        None => {
            return Err(safety::arg_err(format!(
                "{BUILTIN} requires a file path and the content to write"
            )))
        }
    };
    // The content's type is settled here, with the path's, before the path
    // reaches the filesystem: a call with a non-String content must not create a
    // directory entry, a temporary file, or an approval prompt.
    let content = match args.next() {
        Some(Value::Str(content)) => content,
        Some(other) => {
            return Err(safety::bad_arg(
                BUILTIN,
                "content: String",
                other.type_name(),
            ))
        }
        None => {
            return Err(safety::arg_err(format!(
                "{BUILTIN} requires the content to write as its second argument"
            )))
        }
    };
    let options = match args.next() {
        Some(Value::Record(options)) => Some(options),
        Some(other) => {
            return Err(safety::bad_arg(
                BUILTIN,
                "options: Record",
                other.type_name(),
            ))
        }
        None => None,
    };
    if let Some(extra) = args.next() {
        return Err(safety::bad_arg(
            BUILTIN,
            "at most three arguments: path, content and options",
            &format!("a fourth argument of type {}", extra.type_name()),
        ));
    }

    let mut request = Request {
        path,
        content,
        encoding: Choice::Utf8,
        bom: Bom::None,
        max_bytes: DEFAULT_MAX_BYTES as usize,
        expected_sha256: None,
        create_only: false,
    };
    if let Some(options) = options {
        parse_options(&options, &mut request)?;
    }

    // The two preconditions contradict: `create_only` requires the target to be
    // absent and `expected_sha256` requires it to exist and match. No call can
    // satisfy both, so the pair is refused here rather than at the first check.
    if request.create_only && request.expected_sha256.is_some() {
        return Err(safety::bad_arg(
            BUILTIN,
            "create_only and expected_sha256 not to be combined: one requires the \
             target to be absent and the other requires it to exist",
            "both options",
        ));
    }
    Ok(request)
}

fn parse_options(options: &BTreeMap<String, Value>, request: &mut Request) -> Result<()> {
    for key in options.keys() {
        if !OPTION_KEYS.contains(&key.as_str()) {
            return Err(safety::bad_arg(
                BUILTIN,
                &format!(
                    "an options Record whose only keys are {}",
                    OPTION_KEYS.join(", ")
                ),
                &format!("the key {key:?}"),
            ));
        }
    }

    if let Some(value) = options.get("encoding") {
        let label = match value {
            Value::Str(label) => label,
            other => {
                return Err(safety::bad_arg(
                    BUILTIN,
                    ENCODING_EXPECTED,
                    other.type_name(),
                ))
            }
        };
        request.encoding = Choice::parse(label).ok_or_else(|| {
            safety::bad_arg(BUILTIN, ENCODING_EXPECTED, &format!("the label {label:?}"))
        })?;
    }
    if let Some(value) = options.get("bom") {
        let label = match value {
            Value::Str(label) => label,
            other => return Err(safety::bad_arg(BUILTIN, BOM_EXPECTED, other.type_name())),
        };
        request.bom = Bom::parse(label).ok_or_else(|| {
            safety::bad_arg(BUILTIN, BOM_EXPECTED, &format!("the label {label:?}"))
        })?;
    }
    if let Some(value) = options.get("max_bytes") {
        let number = match value {
            Value::Int(number) => *number,
            other => {
                return Err(safety::bad_arg(
                    BUILTIN,
                    MAX_BYTES_EXPECTED,
                    other.type_name(),
                ))
            }
        };
        if !(1..=MAX_MAX_BYTES).contains(&number) {
            return Err(safety::bad_arg(
                BUILTIN,
                MAX_BYTES_EXPECTED,
                &number.to_string(),
            ));
        }
        request.max_bytes = number as usize;
    }
    if let Some(value) = options.get("expected_sha256") {
        let digest = match value {
            Value::Str(digest) => digest,
            other => return Err(safety::bad_arg(BUILTIN, SHA256_EXPECTED, other.type_name())),
        };
        request.expected_sha256 = Some(normalize_sha256(digest)?);
    }
    if let Some(value) = options.get("create_only") {
        match value {
            Value::Bool(flag) => request.create_only = *flag,
            other => {
                return Err(safety::bad_arg(
                    BUILTIN,
                    "create_only to be a Bool",
                    other.type_name(),
                ))
            }
        }
    }
    Ok(())
}

/// Accept an uppercase digest by folding it, and refuse anything that is not 64
/// hexadecimal characters rather than comparing a malformed expectation against
/// a real one and reporting a mismatch.
fn normalize_sha256(digest: &str) -> Result<String> {
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(safety::bad_arg(
            BUILTIN,
            SHA256_EXPECTED,
            &format!("{digest:?}"),
        ));
    }
    Ok(digest.to_ascii_lowercase())
}

// ── encoding ────────────────────────────────────────────────────────────

/// The encoded document, mark first, bounded by `max_bytes`.
fn encode(request: &Request) -> Result<Vec<u8>> {
    let mark = request.bom.mark(request.encoding)?;

    // A cheap upper bound, computed before anything is allocated, so a document
    // the shell would have to allocate past its own ceiling to encode is refused
    // instead. The encoded form never exceeds this, and the ceiling is what
    // bounds the encoder's own allocation.
    let bound = bound_len(request.encoding, &request.content) + mark.len();
    if bound > MAX_MAX_BYTES as usize {
        return Err(safety::budget_exceeded(
            BUILTIN,
            format!(
                "content needs up to {bound} bytes as {}, over the {MAX_MAX_BYTES}-byte ceiling",
                request.encoding.label()
            ),
            "write the document in pieces, or choose an encoding with a smaller expansion",
        ));
    }

    let payload: Vec<u8> = match request.encoding {
        Choice::Utf8 => request.content.as_bytes().to_vec(),
        Choice::Gbk => {
            let (bytes, _, had_errors) = encoding_rs::GBK.encode(&request.content);
            if had_errors {
                let offender = first_unmappable(&request.content)
                    .map(|c| format!("U+{:04X} ({c:?})", c as u32))
                    .unwrap_or_else(|| "a character outside the mapping".to_string());
                return Err(safety::bad_arg(
                    BUILTIN,
                    "content in which every character has a GBK encoding",
                    &offender,
                ));
            }
            bytes.into_owned()
        }
        // Not `encoding_rs`'s UTF-16 encoder: this is the caller's text, and its
        // code units are what the two flavours differ by.
        Choice::Utf16Le | Choice::Utf16Be => {
            let mut units = Vec::with_capacity(bound - mark.len());
            for unit in request.content.encode_utf16() {
                let bytes = match request.encoding {
                    Choice::Utf16Be => unit.to_be_bytes(),
                    _ => unit.to_le_bytes(),
                };
                units.extend_from_slice(&bytes);
            }
            units
        }
    };

    let total = mark.len() + payload.len();
    if total > request.max_bytes {
        return Err(safety::budget_exceeded(
            BUILTIN,
            format!(
                "encoded content is {total} bytes, over the {}-byte max_bytes",
                request.max_bytes
            ),
            "raise max_bytes (ceiling 16777216), or write less",
        ));
    }

    let mut encoded = Vec::with_capacity(total);
    encoded.extend_from_slice(mark);
    encoded.extend_from_slice(&payload);
    Ok(encoded)
}

/// A guaranteed upper bound on the encoded length, without encoding.
///
/// GBK is one byte for ASCII and at most two for anything else — a character it
/// cannot map at all is refused after encoding, not counted here. UTF-16 is two
/// bytes per code unit, which is what `encode_utf16` yields.
fn bound_len(encoding: Choice, content: &str) -> usize {
    match encoding {
        Choice::Utf8 => content.len(),
        Choice::Gbk => content
            .chars()
            .map(|c| if c.is_ascii() { 1 } else { 2 })
            .sum(),
        Choice::Utf16Le | Choice::Utf16Be => content.encode_utf16().count() * 2,
    }
}

/// The first character `GBK` cannot represent, for the refusal's message. Only
/// reached on the failure path, where one extra pass over the content is cheap
/// next to a message that names nothing.
fn first_unmappable(content: &str) -> Option<char> {
    content.chars().find(|c| {
        let mut buffer = [0u8; 4];
        encoding_rs::GBK.encode(c.encode_utf8(&mut buffer)).2
    })
}

fn gbk_bom_refusal() -> anyhow::Error {
    safety::bad_arg(
        BUILTIN,
        "bom: \"add\" with a codec that has no byte-order mark",
        "bom \"add\" with encoding \"gbk\"; use \"none\", or one of the UTF encodings",
    )
}

// ── the target ──────────────────────────────────────────────────────────

/// Validate the resolved path and return the canonical file the write lands on.
///
/// The named path is inspected *before* canonicalization: canonicalizing first
/// would follow a link and quietly retarget the write at whatever it points to,
/// and the caller named a path, not a target behind it. The temporary file is
/// created in this same canonical directory, so the rename stays on one
/// filesystem.
fn validate_target(resolved: &str) -> Result<PathBuf> {
    match fs::symlink_metadata(resolved) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(safety::policy_deny(
                    BUILTIN,
                    format!(
                        "{BUILTIN}: `{resolved}` is a symbolic link, and a write would \
                         replace whatever it points at"
                    ),
                    "pass the link's target explicitly, or remove the link",
                ));
            }
            #[cfg(windows)]
            if is_reparse_point(&meta) {
                return Err(safety::policy_deny(
                    BUILTIN,
                    format!(
                        "{BUILTIN}: `{resolved}` is a reparse point, and a write would \
                         replace whatever it points at"
                    ),
                    "pass the reparse point's target explicitly, or remove it",
                ));
            }
            if !meta.is_file() {
                return Err(safety::bad_arg(
                    BUILTIN,
                    "a regular file, or a path that does not exist yet",
                    &format!("`{resolved}`, which exists and is not a regular file"),
                ));
            }
            if meta.permissions().readonly() {
                // `validate_write_path` refuses this too, in a message an
                // `anyhow` boundary files as `E_UNKNOWN`. Name the condition
                // while the code is still ours to choose.
                return Err(safety::policy_deny(
                    BUILTIN,
                    format!("{BUILTIN}: `{resolved}` is read-only"),
                    "clear the read-only attribute on the target, or name a different \
                     path; the shell did not refuse this, the filesystem would",
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(safety::fs_error(BUILTIN, resolved, &e)),
    }

    // A missing directory is a coded refusal here rather than an opaque
    // canonicalization failure inside path validation.
    let parent = parent_of(Path::new(resolved));
    if !parent.is_dir() {
        return Err(safety::not_found(
            BUILTIN,
            "directory",
            &parent.display().to_string(),
        ));
    }

    // Containment, blocked patterns and the read-only check are the security
    // module's; the shell does not re-implement them here.
    security::validate_write_path(resolved).map_err(|e| write_path_error(resolved, e))
}

/// Give `security::validate_write_path`'s refusals a code.
///
/// Containment, blocked patterns and malformed paths already arrive as
/// `SafetyError`s and are passed through untouched — their specific code is
/// strictly better information than any this could pick. A read-only target, an
/// unusable path and a depth overrun arrive as a bare message instead, which the
/// effect boundary would file as `E_UNKNOWN`: the one code an agent is told to
/// stop on rather than repair. They are the path policy refusing this target, so
/// that is what they are reported as.
fn write_path_error(resolved: &str, error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<safety::SafetyError>().is_some() {
        return safety::ensure_structured(BUILTIN, error);
    }
    safety::policy_deny(
        BUILTIN,
        format!("{BUILTIN}: `{resolved}` cannot be written: {error}"),
        "the path policy refused this target: check its permissions and that every \
         directory on the path exists and is reachable",
    )
}

/// The directory a path lives in, with the empty relative parent read as the
/// current directory the operating system would use.
fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(windows)]
fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    /// `FILE_ATTRIBUTE_REPARSE_POINT`: a junction, a mount point, an OneDrive
    /// placeholder — anything a write would be redirected through.
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

// ── the write, then the read-back ───────────────────────────────────────

/// What the read-back observed.
struct ReadBack {
    matched: bool,
    observed_bytes: Option<i64>,
    observed_sha256: Option<String>,
    error: Option<String>,
}

/// Write `encoded` to `target`, publish it, read it back and report both facts.
///
/// `readback` is the bounded read the verification uses. Production passes
/// [`read_bounded`]; the unit tests below pass a closure that changes the file
/// first, which is the only way to observe what the shell does when another
/// writer wins the race — an ordering a real filesystem cannot be asked for on
/// demand. Nothing else about the call is injectable: the closure is a parameter
/// of a private function, not a setting.
fn write_then_verify<F>(
    target: &Path,
    display: &str,
    encoded: &[u8],
    spec: &Spec,
    readback: F,
) -> Result<Value>
where
    F: FnOnce(&Path, usize) -> io::Result<Vec<u8>>,
{
    if spec.create_only && target.exists() {
        return Err(existing_target_refusal(display));
    }
    if let Some(expected) = &spec.precondition_sha256 {
        check_precondition(target, display, expected, spec.max_bytes)?;
    }

    crate::tx::snapshot(display);

    let mut temp = create_temp(target)?;
    {
        let file = temp.as_file_mut();
        file.write_all(encoded)
            .map_err(|e| safety::fs_error(BUILTIN, target, &e))?;
        file.flush()
            .map_err(|e| safety::fs_error(BUILTIN, target, &e))?;
        file.sync_all()
            .map_err(|e| safety::fs_error(BUILTIN, target, &e))?;
    }

    // A replacement should not silently drop the mode the file had. Best effort:
    // a filesystem that cannot set it is not a reason to refuse a write the
    // caller asked for, and a brand-new target keeps the private mode the
    // temporary file was created with.
    if let Ok(meta) = fs::metadata(target) {
        let _ = temp.as_file().set_permissions(meta.permissions());
    }

    // The second comparison, as late as the shell can make it: between here and
    // the rename is the window a writer outside this process can still win.
    if let Some(expected) = &spec.precondition_sha256 {
        check_precondition(target, display, expected, spec.max_bytes)?;
    }

    let published = if spec.create_only {
        temp.persist_noclobber(target)
    } else {
        temp.persist(target)
    };
    let published = match published {
        Ok(file) => file,
        Err(e) => {
            let error = e.error;
            // Taking the field out of `PersistError` and then letting the rest
            // drop is what removes the temporary file. `forget`-ing it would
            // leave one behind on every failure.
            drop(e.file);
            if spec.create_only && error.kind() == io::ErrorKind::AlreadyExists {
                return Err(existing_target_refusal(display));
            }
            return Err(safety::fs_error(BUILTIN, target, &error));
        }
    };
    // The name is published; the handle is not needed again and an open handle
    // is one more thing a Windows sharing mode can refuse.
    drop(published);

    let readback = match readback(target, spec.max_bytes) {
        Ok(observed) => {
            let difference = verify_bytes(encoded, &observed);
            ReadBack {
                matched: difference.is_none(),
                observed_bytes: Some(observed.len() as i64),
                observed_sha256: Some(sha256_hex(&observed)),
                error: difference,
            }
        }
        Err(e) => ReadBack {
            matched: false,
            observed_bytes: None,
            observed_sha256: None,
            error: Some(format!("read-back failed: {e}")),
        },
    };
    Ok(report(display, spec, encoded, readback))
}

fn existing_target_refusal(display: &str) -> anyhow::Error {
    safety::bad_arg(
        BUILTIN,
        "create_only to name a path that does not exist yet",
        &format!("`{display}`, which already exists"),
    )
}

/// Compare the read-back against what was written.
///
/// `None` means byte-for-byte identical. Anything else is the message the result
/// carries, naming the first difference so a caller is not left guessing whether
/// the length, the mark or the content is the part that disagrees.
fn verify_bytes(expected: &[u8], observed: &[u8]) -> Option<String> {
    if expected == observed {
        return None;
    }
    let at = expected
        .iter()
        .zip(observed.iter())
        .position(|(written, read)| written != read)
        .unwrap_or_else(|| expected.len().min(observed.len()));
    Some(format!(
        "read-back differs: wrote {} bytes (sha256 {}), read {} bytes (sha256 {}); \
         first difference at byte {at}",
        expected.len(),
        sha256_hex(expected),
        observed.len(),
        sha256_hex(observed),
    ))
}

/// Read at most `cap` bytes, refusing a target that is longer.
///
/// A prefix is not a digest. Reading `cap + 1` and keeping the first `cap` would
/// let a caller receive a byte count and a SHA-256 that look like the whole
/// file's, computed over bytes that are not the file — a misleading digest is
/// worse than no digest, because verification exists precisely so a caller can
/// trust the number. A longer target is an error instead, and the caller reports
/// `observed_bytes` and `observed_sha256` as null.
///
/// `ErrorKind::FileTooLarge` is the marker callers branch on: the bounded read
/// has no other way to say "there was more than you allowed", and it is not an
/// I/O fault.
fn read_bounded(path: &Path, cap: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut observed = Vec::new();
    file.take(cap as u64 + 1).read_to_end(&mut observed)?;
    if observed.len() > cap {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "readback exceeded max_bytes; no complete digest available",
        ));
    }
    Ok(observed)
}

/// Apply the shell-wide size policy first, then the caller's own bound.
///
/// The same two limits the reader applies, in the same order. The shell policy
/// is the outer one and a host may lower it at runtime, so reporting it first is
/// truthful when both apply: no `max_bytes` the caller may pass can lift it.
/// Both are budget refusals rather than malformed calls — the request is
/// well-formed and the document or target is simply larger than what may be
/// written or read, which is the distinction that tells a caller to work in
/// pieces instead of correcting an argument.
fn enforce_size(len: u64, max_bytes: usize, display: &str, what: &str) -> Result<()> {
    security::check_file_size_limit(len).map_err(|limit| {
        safety::budget_exceeded(
            BUILTIN,
            format!(
                "{BUILTIN}: `{display}` {what} {len} bytes, over the shell size policy: {limit}"
            ),
            "the shell's configured limit is the host's and no argument to this call can lift \
             it; use a file within that limit",
        )
    })?;
    if len > max_bytes as u64 {
        return Err(safety::budget_exceeded(
            BUILTIN,
            format!(
                "{BUILTIN}: `{display}` {what} {len} bytes, over the max_bytes bound of {max_bytes}"
            ),
            "raise max_bytes within 1 to 16777216, or use a smaller file",
        ));
    }
    Ok(())
}

/// Refuse unless the target's current bytes digest to `expected`.
///
/// The read is bounded by the same `max_bytes` as the write and by the operator's
/// file-size policy, so the precondition cannot be turned into an unbounded
/// allocation — or into a read of a file the native policy forbids — by pointing
/// it at a large target.
fn check_precondition(
    target: &Path,
    display: &str,
    expected: &str,
    max_bytes: usize,
) -> Result<()> {
    // Both limits are checked against the metadata before the content, so a
    // target past either is refused without being loaded at all. The bounded
    // read below covers a file that grows between the two.
    if let Ok(meta) = fs::metadata(target) {
        enforce_size(meta.len(), max_bytes, display, "to read")?;
    }
    let observed = match read_bounded(target, max_bytes) {
        Ok(observed) => observed,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(safety::not_found(BUILTIN, "file", display))
        }
        // The bounded read refuses an oversized target rather than hashing a
        // prefix. What it exceeded is the caller's envelope, so this is a budget
        // refusal and not the `E_IO` a bare read fault would get.
        Err(e) if e.kind() == io::ErrorKind::FileTooLarge => {
            return Err(safety::budget_exceeded(
                BUILTIN,
                format!("`{display}`: {e}"),
                "raise max_bytes (ceiling 16777216) to cover the current file, or \
                 drop expected_sha256",
            ))
        }
        Err(e) => return Err(safety::fs_error(BUILTIN, target, &e)),
    };
    let actual = sha256_hex(&observed);
    if actual != expected {
        return Err(safety::bad_arg(
            BUILTIN,
            &format!("the current sha256 of `{display}`"),
            &format!("{actual} (the call expected {expected})"),
        ));
    }
    Ok(())
}

fn create_temp(target: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = parent_of(target);
    let directory = parent.display().to_string();
    tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(parent)
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                safety::not_found(BUILTIN, "directory", &directory)
            } else {
                safety::fs_error(BUILTIN, parent, &e)
            }
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn report(display: &str, spec: &Spec, encoded: &[u8], readback: ReadBack) -> Value {
    let mut verification = BTreeMap::new();
    verification.insert(
        "method".to_string(),
        Value::Str(VERIFICATION_METHOD.to_string()),
    );
    verification.insert("matched".to_string(), Value::Bool(readback.matched));
    verification.insert(
        "error".to_string(),
        readback.error.map(Value::Str).unwrap_or(Value::Null),
    );

    let mut record = BTreeMap::new();
    record.insert("success".to_string(), Value::Bool(readback.matched));
    record.insert("path".to_string(), Value::Str(display.to_string()));
    // Only reached after `persist` returned `Ok`: the name was published.
    record.insert("written".to_string(), Value::Bool(true));
    record.insert("verified".to_string(), Value::Bool(readback.matched));
    record.insert(
        "encoding".to_string(),
        Value::Str(spec.encoding.label().to_string()),
    );
    record.insert("bom".to_string(), Value::Str(spec.bom.label().to_string()));
    record.insert(
        "expected_bytes".to_string(),
        Value::Int(encoded.len() as i64),
    );
    record.insert(
        "observed_bytes".to_string(),
        readback
            .observed_bytes
            .map(Value::Int)
            .unwrap_or(Value::Null),
    );
    record.insert(
        "expected_sha256".to_string(),
        Value::Str(sha256_hex(encoded)),
    );
    record.insert(
        "observed_sha256".to_string(),
        readback
            .observed_sha256
            .map(Value::Str)
            .unwrap_or(Value::Null),
    );
    record.insert(
        "precondition_sha256".to_string(),
        spec.precondition_sha256
            .clone()
            .map(Value::Str)
            .unwrap_or(Value::Null),
    );
    record.insert("verification".to_string(), Value::Record(verification));
    Value::Record(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(max_bytes: usize) -> Spec {
        Spec {
            encoding: Choice::Utf8,
            bom: Bom::None,
            max_bytes,
            create_only: false,
            precondition_sha256: None,
        }
    }

    fn fields(value: &Value) -> &BTreeMap<String, Value> {
        match value {
            Value::Record(fields) => fields,
            other => panic!("expected a Record, got {}", other.type_name()),
        }
    }

    #[test]
    fn a_writer_that_wins_the_race_is_reported_and_left_alone() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let target = scratch.path().join("raced.txt");
        let encoded = b"ours\n";
        let foreign = b"theirs\n";

        let value = write_then_verify(
            &target,
            "raced.txt",
            encoded,
            &spec(1_048_576),
            |path, cap| {
                // A second writer replaces the file between the rename and the
                // read-back.
                std::fs::write(path, foreign)?;
                read_bounded(path, cap)
            },
        )
        .expect("the publish succeeded; only the verification failed");

        let fields = fields(&value);
        assert_eq!(fields.get("success"), Some(&Value::Bool(false)));
        assert_eq!(fields.get("written"), Some(&Value::Bool(true)));
        assert_eq!(fields.get("verified"), Some(&Value::Bool(false)));
        assert_eq!(
            fields.get("observed_bytes"),
            Some(&Value::Int(foreign.len() as i64))
        );
        assert!(
            matches!(fields.get("verification"), Some(Value::Record(verification))
                if verification.get("error").is_some_and(|e| matches!(e, Value::Str(_)))),
            "the result must say why it is not verified"
        );
        assert_eq!(
            std::fs::read(&target).expect("target readable"),
            foreign,
            "the shell must not undo a writer it does not own"
        );
    }

    #[test]
    fn a_read_back_that_fails_still_reports_the_write() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let target = scratch.path().join("unreadable.txt");
        let encoded = b"written anyway\n";

        let value = write_then_verify(&target, "unreadable.txt", encoded, &spec(64), |_, _| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "simulated read-back failure",
            ))
        })
        .expect("the publish succeeded; the read-back is what failed");

        let fields = fields(&value);
        assert_eq!(fields.get("success"), Some(&Value::Bool(false)));
        assert_eq!(fields.get("written"), Some(&Value::Bool(true)));
        assert_eq!(fields.get("verified"), Some(&Value::Bool(false)));
        assert_eq!(fields.get("observed_bytes"), Some(&Value::Null));
        assert_eq!(fields.get("observed_sha256"), Some(&Value::Null));
        assert_eq!(
            std::fs::read(&target).expect("target readable"),
            encoded,
            "a failed read-back must not remove bytes that were written"
        );
    }

    #[test]
    fn a_read_back_longer_than_the_envelope_reports_no_digest() {
        let scratch = tempfile::tempdir().expect("scratch directory");
        let target = scratch.path().join("grown.txt");
        let encoded = b"ours\n";
        let cap = 64usize;
        let foreign = vec![b'x'; cap + 1];

        let value = write_then_verify(&target, "grown.txt", encoded, &spec(cap), |path, cap| {
            // A second writer appends past the envelope, and the REAL bounded
            // read runs against what is on disk.
            std::fs::write(path, &foreign)?;
            read_bounded(path, cap)
        })
        .expect("the publish succeeded; the read-back is what failed");

        let fields = fields(&value);
        assert_eq!(fields.get("success"), Some(&Value::Bool(false)));
        assert_eq!(fields.get("written"), Some(&Value::Bool(true)));
        assert_eq!(fields.get("verified"), Some(&Value::Bool(false)));
        assert_eq!(
            fields.get("observed_bytes"),
            Some(&Value::Null),
            "a prefix must not be reported as the file's byte count"
        );
        assert_eq!(
            fields.get("observed_sha256"),
            Some(&Value::Null),
            "a prefix must not be reported as the file's digest"
        );
        assert!(
            matches!(fields.get("verification"), Some(Value::Record(verification))
                if verification.get("error").is_some_and(|e| matches!(e, Value::Str(message)
                    if message.contains("no complete digest available")))),
            "the result must say the read was bounded: {value:?}"
        );
        assert_eq!(
            std::fs::read(&target).expect("target readable"),
            foreign,
            "the foreign file must be left exactly as the other writer left it"
        );
    }

    #[test]
    fn verify_bytes_is_none_only_for_identical_bytes() {
        assert_eq!(verify_bytes(b"abc", b"abc"), None);
        let message = verify_bytes(b"abc", b"abd").expect("a difference");
        assert!(
            message.contains("first difference at byte 2"),
            "the message must locate the difference: {message}"
        );
        assert!(
            verify_bytes(b"abc", b"ab").is_some(),
            "a prefix is not the same bytes"
        );
    }
}

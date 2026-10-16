//! `file_read_text` — read one file's bytes once and report the raw snapshot,
//! the byte-order-mark facts and, when a codec was named, the decoded text, as
//! separate fields of one record.
//!
//! The file is opened once and read through that one handle, bounded by
//! `max_bytes`; a file that cannot be read whole is refused rather than reported
//! as a partial snapshot, so `complete` is true in every record this call
//! returns. `bytes`, `base64` and `sha256` describe the full raw bytes — never a
//! stripped or re-encoded form — so the snapshot stays checkable against the
//! file on disk even when decoding fails or a policy removes a mark.
//!
//! Decoding is exact. A named codec is used as named and nothing is sniffed: a
//! byte-order mark that contradicts the named codec makes `decoded` false and
//! leaves the codec alone instead of overriding it, and bytes that are not valid
//! in the named codec make `decoded` false with no U+FFFD substitution. The
//! `bom` encoding is the one that reads a mark instead of being told, and it
//! decodes only a file that begins with the UTF-8 mark `EF BB BF`, the UTF-16LE
//! mark `FF FE`, or the UTF-16BE mark `FE FF`; a file with none of them is a
//! recorded refusal, not a guess at UTF-8. Newlines are never rewritten, so CRLF
//! stays CRLF and a lone CR stays a lone CR.
//!
//! The `bom` option decides what a mark that matches the codec in use means.
//! `preserve` (the default) leaves the mark in the decoded text as U+FEFF,
//! `strip` removes only that matching prefix before decoding, `require` demands
//! the mark and still preserves it, and `forbid` refuses the file when a mark is
//! detected, leaving the raw bytes intact. `bytes` mode returns the raw snapshot
//! and no decoding at all, which is why it accepts only `preserve`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use anyhow::Result;
use encoding_rs::Encoding;

use crate::safety;
use crate::value::Value;

const BUILTIN: &str = "file_read_text";
const DEFAULT_MAX_BYTES: i64 = 1_048_576;
const MAX_BYTES: i64 = 16_777_216;

const ENCODING_EXPECTED: &str = "encoding to be one of \"utf-8\", \"gbk\", \
                                 \"utf-16le\", \"utf-16be\", \"bom\" or \"bytes\"";
const BOM_EXPECTED: &str = "bom to be one of \"preserve\", \"strip\", \"require\" or \"forbid\"";
const MAX_BYTES_EXPECTED: &str = "max_bytes to be an Int from 1 to 16777216";

const BOM_UTF8: [u8; 3] = [0xEF, 0xBB, 0xBF];
const BOM_UTF16LE: [u8; 2] = [0xFF, 0xFE];
const BOM_UTF16BE: [u8; 2] = [0xFE, 0xFF];

/// Read `file_read_text(path, options?)` and answer its result record.
///
/// `input` is refused: the path is the call, and silently reading a piped
/// subject would make `x | file_read_text()` mean something an agent cannot see.
///
/// A malformed call is refused with `E_BAD_ARG` before the filesystem is
/// touched: a path that is not a String, options that are not a Record, an
/// unknown option key or label, a `bom` other than `preserve` beside
/// `encoding: "bytes"`, or a `max_bytes` outside 1 to 16777216. A file that is
/// larger than `max_bytes`, larger than the shell's size policy, or grown past
/// the bound while it is read is `E_BUDGET_EXCEEDED`, because the call is
/// well-formed and the answer is to read less rather than to correct an
/// argument. A path that does not exist is `E_NOT_FOUND`; everything else is the
/// coded filesystem or path-policy error the operation produced.
pub fn run(args: Vec<Value>, input: Option<Value>) -> Result<Value> {
    let request = parse(args, input)?;

    // Resolved first so the path that is checked, sized and opened is one path.
    let resolved = safety::resolve_path_str(&request.path);
    // Absence is answered before `validate_read_path`, whose canonicalization of
    // a missing parent would otherwise arrive as prose under `E_UNKNOWN`: the
    // commonest failure a shell has must stay branchable on its code.
    if !Path::new(&resolved).exists() {
        return Err(safety::not_found(BUILTIN, "file", &resolved));
    }
    let validated = crate::security::validate_read_path(&resolved)?;

    let data = read_snapshot(&validated, request.max_bytes)?;
    Ok(build_record(
        &validated,
        &data,
        request.encoding,
        request.bom,
    ))
}

// ── arguments ───────────────────────────────────────────────────────────

/// The validated call: the path to read and the three option decisions.
struct Request {
    path: String,
    encoding: Choice,
    bom: BomPolicy,
    max_bytes: usize,
}

/// One of the six encodings a caller may name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Utf8,
    Gbk,
    Utf16Le,
    Utf16Be,
    /// Read the mark instead of being told the codec.
    Bom,
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
            "bom" => Self::Bom,
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
            Self::Bom => "bom",
            Self::Bytes => "bytes",
        }
    }

    /// The codec this choice names, or nothing when it names none.
    fn codec(self) -> Option<&'static Encoding> {
        match self {
            Self::Utf8 => Some(encoding_rs::UTF_8),
            Self::Gbk => Some(encoding_rs::GBK),
            Self::Utf16Le => Some(encoding_rs::UTF_16LE),
            Self::Utf16Be => Some(encoding_rs::UTF_16BE),
            Self::Bom | Self::Bytes => None,
        }
    }

    /// How many bytes this choice's own mark occupies at the start of a file.
    fn mark_len(self) -> usize {
        match self {
            Self::Utf8 => BOM_UTF8.len(),
            Self::Utf16Le => BOM_UTF16LE.len(),
            Self::Utf16Be => BOM_UTF16BE.len(),
            Self::Gbk | Self::Bom | Self::Bytes => 0,
        }
    }
}

/// What a detected mark means for the decoded text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BomPolicy {
    Preserve,
    Strip,
    Require,
    Forbid,
}

impl BomPolicy {
    fn parse(label: &str) -> Option<Self> {
        Some(match label {
            "preserve" => Self::Preserve,
            "strip" => Self::Strip,
            "require" => Self::Require,
            "forbid" => Self::Forbid,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Strip => "strip",
            Self::Require => "require",
            Self::Forbid => "forbid",
        }
    }
}

fn parse(args: Vec<Value>, input: Option<Value>) -> Result<Request> {
    if let Some(piped) = input {
        return Err(safety::bad_arg(
            BUILTIN,
            "no piped input: the path is the first argument",
            piped.type_name(),
        ));
    }

    let mut args = args.into_iter();
    let path = match args.next() {
        Some(Value::Str(path)) => path,
        Some(other) => {
            return Err(safety::bad_arg(
                BUILTIN,
                "path as a String",
                other.type_name(),
            ))
        }
        None => return Err(safety::bad_arg(BUILTIN, "path as a String", "nothing")),
    };
    let (encoding, bom, max_bytes) = match args.next() {
        Some(Value::Record(options)) => parse_options(&options)?,
        Some(other) => {
            return Err(safety::bad_arg(
                BUILTIN,
                "options as a Record with only encoding, bom and max_bytes",
                other.type_name(),
            ))
        }
        None => (
            Choice::Utf8,
            BomPolicy::Preserve,
            DEFAULT_MAX_BYTES as usize,
        ),
    };
    if let Some(extra) = args.next() {
        return Err(safety::bad_arg(
            BUILTIN,
            "at most two arguments: path and options",
            &format!("a third argument of type {}", extra.type_name()),
        ));
    }

    Ok(Request {
        path,
        encoding,
        bom,
        max_bytes,
    })
}

fn parse_options(options: &BTreeMap<String, Value>) -> Result<(Choice, BomPolicy, usize)> {
    for key in options.keys() {
        if !matches!(key.as_str(), "encoding" | "bom" | "max_bytes") {
            return Err(safety::bad_arg(
                BUILTIN,
                "an options Record whose only keys are encoding, bom and max_bytes",
                &format!("the key {key:?}"),
            ));
        }
    }

    let encoding = match options.get("encoding") {
        None => Choice::Utf8,
        Some(Value::Str(label)) => Choice::parse(label).ok_or_else(|| {
            safety::bad_arg(BUILTIN, ENCODING_EXPECTED, &format!("the label {label:?}"))
        })?,
        Some(other) => {
            return Err(safety::bad_arg(
                BUILTIN,
                ENCODING_EXPECTED,
                other.type_name(),
            ))
        }
    };

    let bom = match options.get("bom") {
        None => BomPolicy::Preserve,
        Some(Value::Str(label)) => BomPolicy::parse(label).ok_or_else(|| {
            safety::bad_arg(BUILTIN, BOM_EXPECTED, &format!("the label {label:?}"))
        })?,
        Some(other) => return Err(safety::bad_arg(BUILTIN, BOM_EXPECTED, other.type_name())),
    };

    // `bytes` never decodes, so a policy that is about what a decode does with a
    // mark cannot mean anything there. `preserve` is the default and says
    // "report the mark, change nothing", which is exactly what `bytes` does.
    if encoding == Choice::Bytes && bom != BomPolicy::Preserve {
        return Err(safety::bad_arg(
            BUILTIN,
            "no bom option with encoding \"bytes\", which never decodes",
            &format!("bom: {:?}", bom.label()),
        ));
    }

    let max_bytes = match options.get("max_bytes") {
        None => DEFAULT_MAX_BYTES as usize,
        Some(Value::Int(number)) if (1..=MAX_BYTES).contains(number) => *number as usize,
        Some(Value::Int(number)) => {
            return Err(safety::bad_arg(
                BUILTIN,
                MAX_BYTES_EXPECTED,
                &number.to_string(),
            ))
        }
        Some(other) => {
            return Err(safety::bad_arg(
                BUILTIN,
                MAX_BYTES_EXPECTED,
                other.type_name(),
            ))
        }
    };

    Ok((encoding, bom, max_bytes))
}

// ── reading ─────────────────────────────────────────────────────────────

/// Read the whole file through one handle, bounded by the caller's `max_bytes`.
///
/// The size decision is made before anything is read, and from metadata rather
/// than from the bytes: a file the caller cannot have in full is refused before
/// it is loaded. The handle's own metadata is then used for the same decision,
/// because that handle is the file actually read — a path replaced between the
/// two calls is not the file that was sized. That closes this call's own window
/// and is not a fix for path replacement in general; the policy that owns that
/// is the path validation above.
fn read_snapshot(validated: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let path_meta =
        std::fs::metadata(validated).map_err(|e| safety::fs_error(BUILTIN, validated, &e))?;
    refuse_non_file(validated, path_meta.is_file())?;
    enforce_size(path_meta.len(), max_bytes)?;

    let file =
        std::fs::File::open(validated).map_err(|e| safety::fs_error(BUILTIN, validated, &e))?;
    let handle_meta = file
        .metadata()
        .map_err(|e| safety::fs_error(BUILTIN, validated, &e))?;
    refuse_non_file(validated, handle_meta.is_file())?;
    enforce_size(handle_meta.len(), max_bytes)?;

    match read_bounded(file, max_bytes).map_err(|e| safety::fs_error(BUILTIN, validated, &e))? {
        Some(data) => Ok(data),
        None => Err(safety::budget_exceeded(
            BUILTIN,
            format!(
                "the file grew past the {max_bytes}-byte max_bytes bound while it was being read"
            ),
            "retry once the file is stable, or raise max_bytes within 1 to 16777216",
        )),
    }
}

/// Read at most `max_bytes`, answering `None` when the source holds more.
///
/// One byte past the bound is what distinguishes "the file is exactly the bound"
/// from "the file is longer"; stopping at the bound could not tell the two
/// apart. A source that grew after it was sized is caught here rather than
/// reported as a complete short read.
fn read_bounded(source: impl Read, max_bytes: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut data = Vec::new();
    source.take(max_bytes as u64 + 1).read_to_end(&mut data)?;
    Ok((data.len() <= max_bytes).then_some(data))
}

fn refuse_non_file(validated: &Path, is_file: bool) -> Result<()> {
    if is_file {
        return Ok(());
    }
    Err(safety::bad_arg(
        BUILTIN,
        "path to a regular file",
        &format!("the non-file {}", validated.display()),
    ))
}

/// Apply the shell-wide size policy first, then the caller's own bound.
///
/// The shell policy is the outer limit and a host may lower it at runtime; the
/// caller's `max_bytes` is the inner one. Reporting the outer bound first is
/// truthful when both apply, because no `max_bytes` the caller may pass can lift
/// it. Both are budget refusals rather than malformed calls: the request is
/// well-formed and the file is simply larger than what may be read, which is the
/// distinction that tells a caller to read in pieces instead of correcting an
/// argument.
fn enforce_size(len: u64, max_bytes: usize) -> Result<()> {
    crate::security::check_file_size_limit(len).map_err(|limit| {
        safety::budget_exceeded(
            BUILTIN,
            format!("a file of {len} bytes is over the shell size policy: {limit}"),
            "the shell's configured limit is the host's and no argument to this call can lift \
             it; read a file within that limit",
        )
    })?;
    if len > max_bytes as u64 {
        return Err(safety::budget_exceeded(
            BUILTIN,
            format!("a file of {len} bytes is over the max_bytes bound of {max_bytes}"),
            "raise max_bytes within 1 to 16777216, or read the file in pieces with a smaller \
             bound",
        ));
    }
    Ok(())
}

// ── decoding ────────────────────────────────────────────────────────────

/// What decoding produced, or why it produced nothing.
struct Decoded {
    /// The codec name the record reports, when one can be named at all.
    encoding: Option<&'static str>,
    text: Option<String>,
    /// `None` in `bytes` mode: no codec was asked for, so nothing failed.
    decoded: Option<bool>,
    error: Option<String>,
}

/// The mark this data begins with, or nothing.
///
/// Only the three marks this builtin recognizes are detected. A `FF FE 00 00`
/// prefix is read as the UTF-16LE mark followed by a NUL, because UTF-32 is not
/// one of the codecs `file_read_text` offers.
fn detect_bom(data: &[u8]) -> Option<Choice> {
    if data.starts_with(&BOM_UTF8) {
        Some(Choice::Utf8)
    } else if data.starts_with(&BOM_UTF16LE) {
        Some(Choice::Utf16Le)
    } else if data.starts_with(&BOM_UTF16BE) {
        Some(Choice::Utf16Be)
    } else {
        None
    }
}

fn refused(encoding: Option<&'static str>, error: String) -> Decoded {
    Decoded {
        encoding,
        text: None,
        decoded: Some(false),
        error: Some(error),
    }
}

fn decode(data: &[u8], requested: Choice, policy: BomPolicy) -> Decoded {
    if requested == Choice::Bytes {
        return Decoded {
            encoding: Some(Choice::Bytes.label()),
            text: None,
            decoded: None,
            error: None,
        };
    }

    let detected = detect_bom(data);
    let codec =
        match requested {
            Choice::Bytes => unreachable!("bytes mode returned above"),
            Choice::Bom => match detected {
                Some(mark) => mark,
                None => return refused(
                    None,
                    "no recognized byte-order mark: `encoding: \"bom\"` decodes only a file that \
                     begins with the UTF-8 mark EF BB BF, the UTF-16LE mark FF FE, or the UTF-16BE \
                     mark FE FF; name the codec explicitly to read a file that has none"
                        .to_string(),
                ),
            },
            named => match detected {
                // The mark is a fact about the bytes, not a hint that overrides what
                // the caller asked for: a contradiction is reported, and neither the
                // codec nor the garbage it would decode is produced.
                Some(mark) if mark != named => {
                    return refused(
                        Some(named.label()),
                        format!(
                            "the bytes begin with the {} byte-order mark, which contradicts the \
                         requested {}: the named codec was not overridden and nothing was decoded",
                            mark.label(),
                            named.label()
                        ),
                    )
                }
                None if policy == BomPolicy::Require => {
                    return refused(
                        Some(named.label()),
                        format!(
                        "`bom: \"require\"` needs a {} byte-order mark, and the bytes begin with \
                         none",
                        named.label()
                    ),
                    )
                }
                _ => named,
            },
        };

    if policy == BomPolicy::Forbid && detected.is_some() {
        return refused(
            Some(codec.label()),
            format!(
                "`bom: \"forbid\"` refused the {} byte-order mark at the start of the file; the \
                 raw bytes are unchanged",
                codec.label()
            ),
        );
    }

    let skip = match (policy, detected) {
        (BomPolicy::Strip, Some(mark)) => mark.mark_len(),
        _ => 0,
    };
    let body = &data[skip..];
    let Some(encoding) = codec.codec() else {
        unreachable!("only a detected or named codec reaches the decoder")
    };
    match encoding.decode_without_bom_handling_and_without_replacement(body) {
        Some(text) => Decoded {
            encoding: Some(codec.label()),
            text: Some(text.into_owned()),
            decoded: Some(true),
            error: None,
        },
        // Strict by construction: `_and_without_replacement` refuses an invalid
        // sequence instead of substituting U+FFFD, so a real replacement
        // character in the file stays content and a malformed byte does not.
        None => refused(
            Some(codec.label()),
            format!(
                "the {} raw bytes are not valid {}; `base64` and `sha256` hold them unchanged, \
                 and no replacement character was substituted",
                data.len(),
                codec.label()
            ),
        ),
    }
}

// ── the record ──────────────────────────────────────────────────────────

fn build_record(path: &Path, data: &[u8], requested: Choice, policy: BomPolicy) -> Value {
    let detected = detect_bom(data);
    let decoded = decode(data, requested, policy);

    let mut bom = BTreeMap::new();
    bom.insert(
        "detected".to_string(),
        detected
            .map(|mark| Value::Str(mark.label().to_string()))
            .unwrap_or(Value::Null),
    );
    bom.insert(
        "bytes".to_string(),
        Value::Int(detected.map_or(0, |mark| mark.mark_len()) as i64),
    );
    bom.insert("policy".to_string(), Value::Str(policy.label().to_string()));

    let mut record = BTreeMap::new();
    record.insert(
        "success".to_string(),
        Value::Bool(decoded.decoded != Some(false)),
    );
    record.insert(
        "path".to_string(),
        Value::Str(crate::path_display::display_path(path)),
    );
    record.insert("bytes".to_string(), Value::Int(data.len() as i64));
    record.insert("base64".to_string(), Value::Str(base64_standard(data)));
    record.insert("sha256".to_string(), Value::Str(sha256_hex(data)));
    record.insert("complete".to_string(), Value::Bool(true));
    record.insert(
        "requested_encoding".to_string(),
        Value::Str(requested.label().to_string()),
    );
    record.insert(
        "encoding".to_string(),
        decoded
            .encoding
            .map(|name| Value::Str(name.to_string()))
            .unwrap_or(Value::Null),
    );
    record.insert(
        "decoded".to_string(),
        decoded.decoded.map(Value::Bool).unwrap_or(Value::Null),
    );
    record.insert(
        "text".to_string(),
        decoded.text.map(Value::Str).unwrap_or(Value::Null),
    );
    record.insert("bom".to_string(), Value::Record(bom));
    record.insert(
        "error".to_string(),
        decoded.error.map(Value::Str).unwrap_or(Value::Null),
    );
    Value::Record(record)
}

fn base64_standard(data: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD.encode(data)
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(data: &[u8], requested: Choice, policy: BomPolicy) -> Option<String> {
        decode(data, requested, policy).text
    }

    fn mark(data: &[u8]) -> Option<&'static str> {
        detect_bom(data).map(Choice::label)
    }

    #[test]
    fn the_three_recognized_marks_are_detected_and_nothing_else_is() {
        assert_eq!(mark(&[0xEF, 0xBB, 0xBF, b'a']), Some("utf-8"));
        assert_eq!(mark(&[0xFF, 0xFE, b'a', 0x00]), Some("utf-16le"));
        assert_eq!(mark(&[0xFE, 0xFF, 0x00, b'a']), Some("utf-16be"));
        assert_eq!(mark(&[]), None);
        assert_eq!(mark(b"plain"), None);
        // A UTF-32LE mark is not one of the three: it is read as the UTF-16LE
        // mark followed by a NUL, because UTF-32 is not offered here.
        assert_eq!(mark(&[0xFF, 0xFE, 0x00, 0x00]), Some("utf-16le"));
        assert_eq!(mark(&[0xEF, 0xBB]), None, "a truncated mark is not a mark");
    }

    #[test]
    fn preserve_keeps_the_matching_mark_and_strip_removes_only_that_prefix() {
        let cases: [(&[u8], Choice, &str, &[u8]); 3] = [
            (
                &[0xEF, 0xBB, 0xBF, b'h', b'i'],
                Choice::Utf8,
                "hi",
                &[0xEF, 0xBB, 0xBF],
            ),
            (
                &[0xFF, 0xFE, b'A', 0x00],
                Choice::Utf16Le,
                "A",
                &[0xFF, 0xFE],
            ),
            (
                &[0xFE, 0xFF, 0x00, b'A'],
                Choice::Utf16Be,
                "A",
                &[0xFE, 0xFF],
            ),
        ];
        for (data, codec, plain, again) in cases {
            let preserved = format!("\u{feff}{plain}");
            assert_eq!(
                text(data, codec, BomPolicy::Preserve).as_deref(),
                Some(preserved.as_str()),
                "{}: preserve keeps the mark as U+FEFF",
                codec.label()
            );
            assert_eq!(
                text(data, codec, BomPolicy::Strip).as_deref(),
                Some(plain),
                "{}: strip removes only the matching prefix",
                codec.label()
            );
            // Strip removes the prefix, not every U+FEFF in the file: the same
            // mark encoded again at the end is content.
            let mut with_inner = data.to_vec();
            with_inner.extend_from_slice(again);
            let stripped = text(&with_inner, codec, BomPolicy::Strip).expect("strips and decodes");
            assert_eq!(
                stripped,
                format!("{plain}\u{feff}"),
                "{}: only the leading mark is removed",
                codec.label()
            );
        }
    }

    #[test]
    fn require_demands_the_mark_and_still_preserves_it() {
        let marked = [0xEF, 0xBB, 0xBF, b'h', b'i'];
        assert_eq!(
            text(&marked, Choice::Utf8, BomPolicy::Require).as_deref(),
            Some("\u{feff}hi"),
            "require is presence plus preserve, not a strip"
        );
        assert_eq!(text(b"hi", Choice::Utf8, BomPolicy::Require), None);
        let answer = decode(b"hi", Choice::Utf8, BomPolicy::Require);
        assert_eq!(answer.decoded, Some(false));
        assert_eq!(answer.encoding, Some("utf-8"));
        assert!(answer.error.expect("says why").contains("require"));
    }

    #[test]
    fn forbid_refuses_a_detected_mark() {
        let answer = decode(
            &[0xEF, 0xBB, 0xBF, b'h', b'i'],
            Choice::Utf8,
            BomPolicy::Forbid,
        );
        assert_eq!(answer.decoded, Some(false));
        assert_eq!(answer.text, None);
        assert_eq!(answer.encoding, Some("utf-8"));
        assert!(answer.error.expect("says why").contains("forbid"));
        // Without a mark there is nothing to forbid.
        assert_eq!(
            text(b"hi", Choice::Utf8, BomPolicy::Forbid).as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn a_named_codec_is_never_overridden_by_a_contradicting_mark() {
        let clash = [0xFE, 0xFF, 0x00, b'A'];
        for codec in [Choice::Utf8, Choice::Gbk, Choice::Utf16Le] {
            let answer = decode(&clash, codec, BomPolicy::Preserve);
            assert_eq!(answer.decoded, Some(false), "{}", codec.label());
            assert_eq!(answer.text, None, "{}", codec.label());
            assert_eq!(
                answer.encoding,
                Some(codec.label()),
                "{}: the requested name stays reported",
                codec.label()
            );
            let error = answer.error.expect("says why");
            assert!(error.contains("contradicts"), "{}: {error}", codec.label());
        }
        // The codec that matches the mark is not a contradiction.
        assert_eq!(
            text(&clash, Choice::Utf16Be, BomPolicy::Strip).as_deref(),
            Some("A")
        );
    }

    #[test]
    fn bom_encoding_requires_a_mark_and_names_the_one_it_found() {
        let without = decode(b"plain utf-8", Choice::Bom, BomPolicy::Preserve);
        assert_eq!(without.decoded, Some(false));
        assert_eq!(without.encoding, None, "nothing was detected to name");
        assert_eq!(without.text, None);

        for (data, name, expected) in [
            (&[0xEF, 0xBB, 0xBF, b'h', b'i'][..], "utf-8", "\u{feff}hi"),
            (&[0xFF, 0xFE, b'A', 0x00][..], "utf-16le", "\u{feff}A"),
            (&[0xFE, 0xFF, 0x00, b'A'][..], "utf-16be", "\u{feff}A"),
        ] {
            let answer = decode(data, Choice::Bom, BomPolicy::Preserve);
            assert_eq!(answer.decoded, Some(true), "{name}");
            assert_eq!(answer.encoding, Some(name));
            assert_eq!(answer.text.as_deref(), Some(expected), "{name}");
            let stripped = decode(data, Choice::Bom, BomPolicy::Strip);
            assert_eq!(
                stripped.text.as_deref(),
                Some(expected.trim_start_matches('\u{feff}')),
                "{name}: strip removes the detected mark"
            );
        }
    }

    #[test]
    fn bytes_mode_reports_no_decoding_at_all() {
        let answer = decode(
            &[0xEF, 0xBB, 0xBF, 0xFF],
            Choice::Bytes,
            BomPolicy::Preserve,
        );
        assert_eq!(answer.decoded, None);
        assert_eq!(answer.text, None);
        assert_eq!(answer.encoding, Some("bytes"));
        assert_eq!(answer.error, None);
    }

    #[test]
    fn gbk_decodes_only_when_gbk_is_named() {
        // `中文测试` in GBK: not valid UTF-8.
        let gbk: &[u8] = &[0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4];
        assert_eq!(
            text(gbk, Choice::Gbk, BomPolicy::Preserve).as_deref(),
            Some("中文测试")
        );
        let utf8 = decode(gbk, Choice::Utf8, BomPolicy::Preserve);
        assert_eq!(utf8.decoded, Some(false));
        assert_eq!(utf8.text, None);
        assert_eq!(utf8.encoding, Some("utf-8"));
    }

    #[test]
    fn strict_decoding_rejects_malformed_bytes_and_keeps_a_real_replacement() {
        // A bare continuation byte, a truncated two-byte sequence, a CESU-8
        // surrogate, and an odd-length UTF-16 tail.
        for (data, codec) in [
            (&[0x80][..], Choice::Utf8),
            (&[0x41, 0xC3, 0x28, 0x42][..], Choice::Utf8),
            (&[0xED, 0xA0, 0x80][..], Choice::Utf8),
            (&[0x41, 0x00, 0x42][..], Choice::Utf16Le),
        ] {
            let answer = decode(data, codec, BomPolicy::Preserve);
            assert_eq!(answer.decoded, Some(false), "{data:?}");
            assert_eq!(answer.text, None, "{data:?}");
            assert!(answer.error.is_some(), "{data:?}");
        }
        // U+FFFD really present in the file, encoded as EF BF BD, is content and
        // must not be mistaken for a substituted one.
        let real = decode(&[0xEF, 0xBF, 0xBD], Choice::Utf8, BomPolicy::Preserve);
        assert_eq!(real.decoded, Some(true));
        assert_eq!(real.text.as_deref(), Some("\u{fffd}"));
        // The UTF-8 mark followed by a real U+FFFD.
        let both = decode(
            &[0xEF, 0xBB, 0xBF, 0xEF, 0xBF, 0xBD],
            Choice::Utf8,
            BomPolicy::Strip,
        );
        assert_eq!(both.decoded, Some(true));
        assert_eq!(both.text.as_deref(), Some("\u{fffd}"));
    }

    #[test]
    fn newlines_and_control_bytes_are_never_rewritten() {
        let raw = b"a\r\nb\nc\rd\te\0f\\g${h}";
        assert_eq!(
            text(raw, Choice::Utf8, BomPolicy::Preserve).as_deref(),
            Some("a\r\nb\nc\rd\te\0f\\g${h}")
        );
        // A BOM elsewhere than the start is content, not a mark.
        assert_eq!(mark(b"a\xEF\xBB\xBFb"), None);
        assert_eq!(
            text(b"a\xEF\xBB\xBFb", Choice::Utf8, BomPolicy::Require),
            None,
            "a mark that is not a prefix does not satisfy require"
        );
    }

    #[test]
    fn the_size_policy_and_the_callers_bound_are_both_enforced() {
        // The shell policy is checked first, at a size no configured policy can
        // accept. An oversized read is a budget refusal, not a malformed call:
        // the argument is well-formed and the file is simply too large.
        let policy = enforce_size(512 * 1024 * 1024, MAX_BYTES as usize)
            .expect_err("the shell policy must refuse half a gigabyte");
        let message = policy.to_string();
        assert!(
            message.contains("E_BUDGET_EXCEEDED"),
            "not coded: {message}"
        );
        assert!(!message.contains("E_BAD_ARG"), "{message}");
        assert!(message.contains("shell size policy"), "{message}");

        // Then the caller's bound, at the largest legal value and just past it.
        assert!(enforce_size(MAX_BYTES as u64, MAX_BYTES as usize).is_ok());
        let bound = enforce_size(MAX_BYTES as u64 + 1, MAX_BYTES as usize)
            .expect_err("one byte past max_bytes must be refused");
        let message = bound.to_string();
        assert!(
            message.contains("E_BUDGET_EXCEEDED"),
            "not coded: {message}"
        );
        assert!(!message.contains("E_BAD_ARG"), "{message}");
        assert!(message.contains("max_bytes"), "{message}");
        assert!(
            enforce_size(0, 1).is_ok(),
            "an empty file is within any bound"
        );
    }

    #[test]
    fn the_bounded_read_keeps_exactly_the_bound_and_refuses_a_longer_source() {
        use std::io::Cursor;

        let exact = read_bounded(Cursor::new(vec![7u8; 8]), 8)
            .expect("a cursor reads")
            .expect("eight bytes are within eight");
        assert_eq!(exact.len(), 8);
        assert!(
            read_bounded(Cursor::new(vec![7u8; 9]), 8)
                .expect("a cursor reads")
                .is_none(),
            "a source one byte past the bound is refused, never truncated"
        );
        assert!(read_bounded(Cursor::new(Vec::new()), 1)
            .expect("a cursor reads")
            .is_some());
    }

    #[test]
    fn a_malformed_call_is_refused_before_any_file_is_looked_for() {
        let options = |pairs: &[(&str, Value)]| {
            Value::Record(
                pairs
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), value.clone()))
                    .collect(),
            )
        };
        for (args, why) in [
            (vec![Value::Int(1)], "path is not a String"),
            (vec![], "no path at all"),
            (
                vec![
                    Value::Str("x".into()),
                    options(&[("unknown", Value::Int(1))]),
                ],
                "an unknown option key",
            ),
            (
                vec![
                    Value::Str("x".into()),
                    options(&[("encoding", Value::Str("latin-1".into()))]),
                ],
                "an unknown encoding label",
            ),
            (
                vec![
                    Value::Str("x".into()),
                    options(&[("bom", Value::Str("drop".into()))]),
                ],
                "an unknown bom label",
            ),
            (
                vec![
                    Value::Str("x".into()),
                    options(&[
                        ("encoding", Value::Str("bytes".into())),
                        ("bom", Value::Str("strip".into())),
                    ]),
                ],
                "bom with bytes mode",
            ),
            (
                vec![
                    Value::Str("x".into()),
                    options(&[("max_bytes", Value::Int(0))]),
                ],
                "zero max_bytes",
            ),
            (
                vec![
                    Value::Str("x".into()),
                    options(&[("max_bytes", Value::Int(16_777_217))]),
                ],
                "max_bytes past the cap",
            ),
            (
                vec![
                    Value::Str("x".into()),
                    options(&[("max_bytes", Value::Str("big".into()))]),
                ],
                "max_bytes is not an Int",
            ),
            (
                vec![Value::Str("x".into()), options(&[]), options(&[])],
                "a third argument",
            ),
        ] {
            let error = parse(args, None).err().expect(why);
            assert!(error.to_string().contains("E_BAD_ARG"), "{why}: {error}");
        }

        let piped = parse(
            vec![Value::Str("x".into())],
            Some(Value::Str("piped".into())),
        )
        .err()
        .expect("a piped subject must be refused");
        assert!(piped.to_string().contains("E_BAD_ARG"), "{piped}");
        assert!(piped.to_string().contains("piped"), "{piped}");

        // `bytes` with the default (omitted) policy is the accepted spelling.
        assert!(parse(
            vec![
                Value::Str("x".into()),
                options(&[("encoding", Value::Str("bytes".into()))]),
            ],
            None
        )
        .is_ok());
    }

    #[test]
    fn the_record_carries_exactly_the_frozen_fields() {
        let Value::Record(record) = build_record(
            Path::new("probe.txt"),
            b"abc",
            Choice::Bytes,
            BomPolicy::Preserve,
        ) else {
            panic!("the result must be a Record")
        };
        let keys: Vec<&str> = record.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "base64",
                "bom",
                "bytes",
                "complete",
                "decoded",
                "encoding",
                "error",
                "path",
                "requested_encoding",
                "sha256",
                "success",
                "text",
            ]
        );
        assert_eq!(record["bytes"], Value::Int(3));
        assert_eq!(record["base64"], Value::Str("YWJj".to_string()));
        assert_eq!(
            record["sha256"],
            Value::Str("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()),
            "lowercase hex of the raw bytes"
        );
        assert_eq!(record["success"], Value::Bool(true));
        assert_eq!(record["complete"], Value::Bool(true));
        assert_eq!(record["decoded"], Value::Null);
        assert_eq!(record["text"], Value::Null);
        assert_eq!(record["encoding"], Value::Str("bytes".into()));
        assert_eq!(record["requested_encoding"], Value::Str("bytes".into()));
        let Value::Record(bom) = &record["bom"] else {
            panic!("bom must be a Record")
        };
        assert_eq!(bom["detected"], Value::Null);
        assert_eq!(bom["bytes"], Value::Int(0));
        assert_eq!(bom["policy"], Value::Str("preserve".into()));

        // The mark is reported from the raw bytes even in bytes mode, and the
        // count is the mark's own length, not the file's.
        let Value::Record(marked) = build_record(
            Path::new("probe.txt"),
            &[0xEF, 0xBB, 0xBF, b'x'],
            Choice::Bytes,
            BomPolicy::Preserve,
        ) else {
            panic!("the result must be a Record")
        };
        let Value::Record(bom) = &marked["bom"] else {
            panic!("bom must be a Record")
        };
        assert_eq!(bom["detected"], Value::Str("utf-8".into()));
        assert_eq!(bom["bytes"], Value::Int(3));
        assert_eq!(
            marked["bytes"],
            Value::Int(4),
            "the snapshot is the whole file"
        );

        // The helpers hash and encode the raw bytes, independently of the record.
        assert_eq!(base64_standard(&[]), "");
        assert_eq!(base64_standard(&[0xD6, 0xD0]), "1tA=");
        assert_eq!(
            sha256_hex(&[]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(
            sha256_hex(b"abc").chars().all(|c| !c.is_ascii_uppercase()),
            "the digest is lower-case hex"
        );
    }
}

//! `walk` — one bounded, pattern-filtered directory tree as a single record.
//!
//! `walk(path, options?)` lists the entries below one directory and states what
//! the traversal did: the entries reported, whether it finished, why it stopped,
//! how much it visited, and the failures it met. `find(path, pattern?)` keeps its
//! `Array` and `fs_walk(path)` keeps its unbounded recursive `Array`.
//!
//! Every argument decision is made before the filesystem is touched — the
//! options record is validated and every pattern compiled before the root is
//! resolved — so a malformed call is `E_BAD_ARG` even when the path is absent.
//! `docs/api/TRAVERSAL.md` owns the option and result contract.
//!
//! Symbolic links below the resolved root are never followed or reported; each is counted in
//! `skipped_links`. Entries arrive in the order the filesystem yields them,
//! which is not specified and not sorted: sorting a directory is the step that
//! needs every name in memory at once. `complete` is true only when the walk
//! reached the end of the tree without a limit stop and without a failure.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use walkdir::{DirEntry, WalkDir};

use crate::safety::{self, ErrorCode};
use crate::value::Value;

const BUILTIN: &str = "walk";

/// Defaults and upper bounds: depth below the root, reported entries, entries
/// visited, and error records kept.
const DEFAULT_MAX_DEPTH: i64 = 64;
const MAX_MAX_DEPTH: i64 = 4_096;
const DEFAULT_MAX_RESULTS: i64 = 1_000;
const MAX_MAX_RESULTS: i64 = 1_000_000;
const DEFAULT_MAX_ENTRIES: i64 = 100_000;
const MAX_MAX_ENTRIES: i64 = 10_000_000;
const DEFAULT_MAX_ERRORS: i64 = 32;
const MAX_MAX_ERRORS: i64 = 1_000;
/// Bounds on one pattern, and on one pattern list.
const MAX_PATTERN_BYTES: usize = 4_096;
const MAX_PATTERNS: usize = 1_024;

const OPTIONS_EXPECTED: &str = "options as a Record with only include, exclude, max_depth, \
                                max_results, max_entries, max_errors, include_dirs and \
                                modified_after_ms";
const PATTERN_EXPECTED: &str = "a relative glob pattern: `/` separators only, at most 4096 \
                                bytes, no absolute or drive prefix, no `.` or `..` segment, no \
                                backslash";
const MODIFIED_AFTER_EXPECTED: &str =
    "modified_after_ms to be an Int of milliseconds since the UTC epoch, at or after 0";

/// A malformed-argument refusal for this builtin.
fn bad(expected: &str, got: &str) -> anyhow::Error {
    safety::bad_arg(BUILTIN, expected, got)
}

/// Walk `walk(path, options?)` and answer its result record.
///
/// `input` is refused: the path is the call, and silently walking a piped
/// subject would make `x | walk()` mean something an agent cannot see.
///
/// A path that does not exist is `E_NOT_FOUND`, a path that is not a directory
/// is `E_BAD_ARG`, and every other failure to reach the root is the coded
/// filesystem or path-policy error the operation produced. A failure *inside*
/// the tree is not an error from this call: it is recorded in the result's
/// `errors`, the walk continues, and `complete` is false.
pub fn run(args: Vec<Value>, input: Option<Value>) -> Result<Value> {
    let request = parse(args, input)?;

    // Resolved first so the path that is checked is the path that is walked.
    let resolved = safety::resolve_path_str(&request.path);
    let metadata = std::fs::metadata(&resolved)
        .map_err(|error| safety::fs_error(BUILTIN, &resolved, &error))?;
    if !metadata.is_dir() {
        return Err(bad(
            "path to a directory",
            &format!("the non-directory {resolved}"),
        ));
    }
    let root = crate::security::validate_read_path(&resolved)?;

    Ok(Value::Record(traverse(&request, &root)))
}

// ── the validated call ──────────────────────────────────────────────────

/// The validated call: the root and every traversal decision, with no
/// filesystem fact in it yet.
struct Request {
    path: String,
    include: Vec<Glob>,
    exclude: Vec<Glob>,
    max_depth: usize,
    max_results: usize,
    max_entries: usize,
    max_errors: usize,
    include_dirs: bool,
    modified_after_ms: Option<i64>,
}

impl Request {
    /// Whether an entry is dropped by `exclude`.
    fn excluded(&self, relative: &str) -> Result<bool> {
        for glob in &self.exclude {
            if glob.matches(relative)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether an entry is reported by `include`; with no patterns, all are.
    fn included(&self, relative: &str) -> Result<bool> {
        if self.include.is_empty() {
            return Ok(true);
        }
        for glob in &self.include {
            if glob.matches(relative)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn parse(args: Vec<Value>, input: Option<Value>) -> Result<Request> {
    if let Some(piped) = input {
        return Err(bad(
            "no piped input: the path is the first argument",
            piped.type_name(),
        ));
    }

    let mut args = args.into_iter();
    let path = match args.next() {
        Some(Value::Str(path)) => path,
        Some(other) => {
            return Err(bad(
                "path as a String naming a directory",
                other.type_name(),
            ))
        }
        None => return Err(bad("path as a String naming a directory", "nothing")),
    };

    let mut request = Request {
        path,
        include: Vec::new(),
        exclude: Vec::new(),
        max_depth: DEFAULT_MAX_DEPTH as usize,
        max_results: DEFAULT_MAX_RESULTS as usize,
        max_entries: DEFAULT_MAX_ENTRIES as usize,
        max_errors: DEFAULT_MAX_ERRORS as usize,
        include_dirs: false,
        modified_after_ms: None,
    };
    match args.next() {
        Some(Value::Record(options)) => apply_options(&mut request, &options)?,
        Some(other) => return Err(bad(OPTIONS_EXPECTED, other.type_name())),
        None => {}
    }
    if let Some(extra) = args.next() {
        return Err(bad(
            "at most two arguments: path and options",
            &format!("a third argument of type {}", extra.type_name()),
        ));
    }
    Ok(request)
}

fn apply_options(request: &mut Request, options: &BTreeMap<String, Value>) -> Result<()> {
    for key in options.keys() {
        if !matches!(
            key.as_str(),
            "include"
                | "exclude"
                | "max_depth"
                | "max_results"
                | "max_entries"
                | "max_errors"
                | "include_dirs"
                | "modified_after_ms"
        ) {
            return Err(bad(OPTIONS_EXPECTED, &format!("the key {key:?}")));
        }
    }

    if let Some(value) = options.get("include") {
        request.include = patterns(value, "include")?;
    }
    if let Some(value) = options.get("exclude") {
        request.exclude = patterns(value, "exclude")?;
    }
    if let Some(value) = options.get("include_dirs") {
        match value {
            Value::Bool(flag) => request.include_dirs = *flag,
            other => return Err(bad("include_dirs to be a Bool", other.type_name())),
        }
    }
    request.max_depth = int_option(options, "max_depth", 0, MAX_MAX_DEPTH, DEFAULT_MAX_DEPTH)?;
    request.max_results = int_option(
        options,
        "max_results",
        1,
        MAX_MAX_RESULTS,
        DEFAULT_MAX_RESULTS,
    )?;
    request.max_entries = int_option(
        options,
        "max_entries",
        1,
        MAX_MAX_ENTRIES,
        DEFAULT_MAX_ENTRIES,
    )?;
    request.max_errors = int_option(options, "max_errors", 1, MAX_MAX_ERRORS, DEFAULT_MAX_ERRORS)?;

    if let Some(value) = options.get("modified_after_ms") {
        match value {
            Value::Int(milliseconds) if *milliseconds >= 0 => {
                request.modified_after_ms = Some(*milliseconds)
            }
            Value::Int(milliseconds) => {
                return Err(bad(MODIFIED_AFTER_EXPECTED, &milliseconds.to_string()))
            }
            other => return Err(bad(MODIFIED_AFTER_EXPECTED, other.type_name())),
        }
    }
    Ok(())
}

/// One bounded option, defaulted and refused outside its range.
fn int_option(
    options: &BTreeMap<String, Value>,
    key: &str,
    min: i64,
    max: i64,
    default: i64,
) -> Result<usize> {
    let expected = format!("{key} to be an Int from {min} to {max}");
    match options.get(key) {
        None => Ok(default as usize),
        Some(Value::Int(number)) if (min..=max).contains(number) => Ok(*number as usize),
        Some(Value::Int(number)) => Err(bad(&expected, &number.to_string())),
        Some(other) => Err(bad(&expected, other.type_name())),
    }
}

/// One pattern list, compiled whole before any of it is used.
fn patterns(value: &Value, key: &str) -> Result<Vec<Glob>> {
    let Value::Array(items) = value else {
        return Err(bad(
            &format!("{key} to be an Array of String patterns"),
            value.type_name(),
        ));
    };
    if items.len() > MAX_PATTERNS {
        return Err(bad(
            &format!("{key} to hold at most {MAX_PATTERNS} patterns"),
            &items.len().to_string(),
        ));
    }
    let mut globs = Vec::with_capacity(items.len());
    for item in items {
        let Value::Str(pattern) = item else {
            return Err(bad(
                &format!("every {key} pattern to be a String"),
                item.type_name(),
            ));
        };
        globs.push(Glob::compile(pattern)?);
    }
    Ok(globs)
}

// ── patterns ────────────────────────────────────────────────────────────

/// One compiled pattern, matched against an entry's `/`-separated path
/// relative to the walk root. `*` and `?` never cross a `/`, and a `**`
/// segment alone matches zero or more whole segments. A leading `.` is an
/// ordinary character, and matching is byte-for-byte on `relative_path`.
struct Glob {
    segments: Vec<Segment>,
}

enum Segment {
    /// `**`: zero or more whole path segments.
    AnyDepth,
    /// One path segment, described by its own tokens.
    Chars(Vec<Token>),
}

enum Token {
    /// `*`: any run of characters within the segment.
    Any,
    /// `?`: exactly one character within the segment.
    One,
    /// `[...]`: one character from the listed ranges, or outside them when
    /// negated by a leading `!` or `^`.
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    /// Any other character, matched exactly.
    Literal(char),
}

impl Glob {
    /// Compile one pattern, or refuse it with the reason it cannot mean
    /// anything here.
    fn compile(pattern: &str) -> Result<Self> {
        let refuse =
            |why: String| bad(PATTERN_EXPECTED, &format!("the pattern {pattern:?}: {why}"));
        if pattern.is_empty() {
            return Err(refuse("it is empty".to_string()));
        }
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(refuse(format!(
                "it is {} bytes, over the {MAX_PATTERN_BYTES}-byte bound",
                pattern.len()
            )));
        }
        if pattern.contains('\0') {
            return Err(refuse("it contains a NUL".to_string()));
        }
        // A backslash is refused rather than read as an escape: on Windows it is
        // the separator the caller probably wrote, and an escape would silently
        // change what the pattern matches.
        if pattern.contains('\\') {
            return Err(refuse(
                "it contains a backslash; write `/` on every platform".to_string(),
            ));
        }
        if pattern.starts_with('/') {
            return Err(refuse(
                "it is absolute; patterns are relative to the walk root".to_string(),
            ));
        }
        if pattern.as_bytes()[1..].first() == Some(&b':') {
            return Err(refuse(
                "it begins with a drive prefix; patterns are relative to the walk root".to_string(),
            ));
        }

        let mut segments = Vec::new();
        for text in pattern.split('/') {
            if text.is_empty() {
                return Err(refuse(
                    "it has an empty path segment; a pattern has no leading, trailing or doubled \
                     `/`"
                    .to_string(),
                ));
            }
            if text == "." || text == ".." {
                return Err(refuse(format!(
                    "it has a {text:?} segment; a pattern is matched below the walk root"
                )));
            }
            segments.push(if text == "**" {
                Segment::AnyDepth
            } else {
                Segment::Chars(tokens(text).map_err(refuse)?)
            });
        }
        Ok(Self { segments })
    }

    /// Whether this pattern matches one `/`-separated relative path.
    ///
    /// A two-row table rather than recursion, so a pattern of many `**`
    /// segments costs the product of the two segment counts and cannot blow up
    /// on a deep path.
    fn matches(&self, relative: &str) -> Result<bool> {
        let path: Vec<&str> = relative.split('/').collect();
        let mut current = vec![false; path.len() + 1];
        let mut next = vec![false; path.len() + 1];
        current[0] = true;
        for segment in &self.segments {
            safety::check_deadline()?;
            next.fill(false);
            match segment {
                Segment::AnyDepth => {
                    let mut reachable = false;
                    for (index, matched) in current.iter().enumerate() {
                        reachable |= *matched;
                        next[index] = reachable;
                    }
                }
                Segment::Chars(tokens) => {
                    for index in 1..=path.len() {
                        next[index] = current[index - 1] && match_segment(tokens, path[index - 1]);
                    }
                }
            }
            std::mem::swap(&mut current, &mut next);
        }
        Ok(current[path.len()])
    }
}

/// The tokens of one pattern segment.
fn tokens(segment: &str) -> std::result::Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = segment.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '*' => tokens.push(Token::Any),
            '?' => tokens.push(Token::One),
            '[' => {
                let negated = matches!(chars.peek(), Some(&'!') | Some(&'^'));
                if negated {
                    chars.next();
                }
                let mut ranges: Vec<(char, char)> = Vec::new();
                let mut closed = false;
                while let Some(member) = chars.next() {
                    if member == ']' {
                        closed = true;
                        break;
                    }
                    // A range needs a member after the `-`; a trailing `-` and
                    // the `]` itself are ordinary members.
                    let mut lookahead = chars.clone();
                    if lookahead.next() == Some('-') {
                        if let Some(&high) = lookahead.peek() {
                            if high != ']' {
                                chars.next();
                                chars.next();
                                if (high as u32) < (member as u32) {
                                    return Err(format!(
                                        "the range {member:?}-{high:?} runs backwards"
                                    ));
                                }
                                ranges.push((member, high));
                                continue;
                            }
                        }
                    }
                    ranges.push((member, member));
                }
                if !closed {
                    return Err(format!("the `[` in {segment:?} is never closed"));
                }
                if ranges.is_empty() {
                    return Err(format!("the character class in {segment:?} lists nothing"));
                }
                tokens.push(Token::Class { negated, ranges });
            }
            other => tokens.push(Token::Literal(other)),
        }
    }
    Ok(tokens)
}

impl Token {
    /// Whether this token accepts one character. `Any` answers here too; the
    /// matcher handles it as a star before asking.
    fn accepts(&self, character: char) -> bool {
        match self {
            Token::Any | Token::One => true,
            Token::Literal(wanted) => *wanted == character,
            Token::Class { negated, ranges } => {
                let listed = ranges
                    .iter()
                    .any(|(low, high)| (*low..=*high).contains(&character));
                listed != *negated
            }
        }
    }
}

/// Whether one segment pattern matches one segment, with `*` backtracking.
fn match_segment(tokens: &[Token], segment: &str) -> bool {
    let text: Vec<char> = segment.chars().collect();
    let (mut token, mut at) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while at < text.len() {
        if token < tokens.len() {
            if matches!(tokens[token], Token::Any) {
                star = Some((token + 1, at));
                token += 1;
                continue;
            }
            if tokens[token].accepts(text[at]) {
                token += 1;
                at += 1;
                continue;
            }
        }
        match star {
            Some((after, from)) => {
                token = after;
                at = from + 1;
                star = Some((after, at));
            }
            None => return false,
        }
    }
    tokens[token..].iter().all(|t| matches!(t, Token::Any))
}

// ── the traversal ───────────────────────────────────────────────────────

/// Why the walk stopped before the end of the tree.
enum Stop {
    Deadline,
    EntryLimit,
    ResultLimit,
}

/// What the walk has to say, accumulated in one place so the loop reads as the
/// traversal rather than as bookkeeping. `visited` counts the root and every
/// entry the walk yielded; a pruned subtree is not visited.
struct Report {
    entries: Vec<Value>,
    errors: Vec<Value>,
    error_count: i64,
    visited: i64,
    matched: i64,
    skipped_links: i64,
    max_errors: usize,
    stop: Option<Stop>,
}

impl Report {
    fn new(max_errors: usize) -> Self {
        Self {
            entries: Vec::new(),
            errors: Vec::new(),
            error_count: 0,
            visited: 0,
            matched: 0,
            skipped_links: 0,
            max_errors,
            stop: None,
        }
    }

    /// Record one failure against `path`, keeping at most `max_errors` records
    /// while still counting every one.
    fn note_coded(&mut self, path: &Path, code: &str, message: String) {
        self.error_count += 1;
        if self.errors.len() >= self.max_errors {
            return;
        }
        let mut record = BTreeMap::new();
        record.insert(
            "path".to_string(),
            Value::Str(crate::path_display::display_path(path)),
        );
        record.insert("code".to_string(), Value::Str(code.to_string()));
        record.insert("message".to_string(), Value::Str(message));
        self.errors.push(Value::Record(record));
    }

    /// Record a filesystem failure under the code `safety::fs_error` gives it
    /// everywhere else in the shell.
    fn note_io(&mut self, path: &Path, error: &std::io::Error) {
        let described = safety::fs_error(BUILTIN, path, error);
        match described.downcast_ref::<safety::SafetyError>() {
            Some(described) => {
                self.note_coded(path, described.code.as_str(), described.message.clone())
            }
            None => self.note_coded(path, ErrorCode::Io.as_str(), error.to_string()),
        }
    }

    /// Record a `walkdir` failure, which carries its own path when it has one.
    fn note_error(&mut self, error: &walkdir::Error, fallback: &Path) {
        let path = error.path().unwrap_or(fallback);
        match error.io_error() {
            Some(io) => self.note_io(path, io),
            None => self.note_coded(path, ErrorCode::Io.as_str(), format!("{BUILTIN}: {error}")),
        }
    }

    /// Record that the call's deadline cancelled the walk. Work already done is
    /// reported rather than discarded, and the code says why it is not the
    /// whole tree.
    fn note_deadline(&mut self, root: &Path, cancelled: &anyhow::Error) {
        self.note_coded(
            root,
            ErrorCode::BudgetExceeded.as_str(),
            format!("{BUILTIN}: the traversal stopped at the call's deadline: {cancelled}"),
        );
    }

    /// Record a path that cannot be reported as a String. A lossy rendering
    /// would invent a name that need not exist and could collide with a real
    /// one, so no entry is reported for it; the `path` field below is that
    /// rendering, present only so the failure has a location.
    fn note_unrepresentable(&mut self, path: &Path) {
        self.note_coded(
            path,
            ErrorCode::Io.as_str(),
            format!(
                "{BUILTIN}: the name of `{}` is not valid UTF-8, so no entry is reported for it \
                 and no substitute name is invented; this path is a lossy rendering for location \
                 only",
                path.to_string_lossy()
            ),
        );
    }

    /// Report one entry.
    fn push_entry(
        &mut self,
        entry: &DirEntry,
        relative: &str,
        is_dir: bool,
        size: i64,
        modified_ms: Option<i64>,
    ) {
        let name = relative.rsplit('/').next().unwrap_or(relative);
        let mut record = BTreeMap::new();
        record.insert(
            "path".to_string(),
            Value::Str(crate::path_display::display_path(entry.path())),
        );
        record.insert(
            "relative_path".to_string(),
            Value::Str(relative.to_string()),
        );
        record.insert("name".to_string(), Value::Str(name.to_string()));
        record.insert("is_dir".to_string(), Value::Bool(is_dir));
        record.insert("size".to_string(), Value::Int(size));
        record.insert(
            "modified_ms".to_string(),
            modified_ms.map_or(Value::Null, Value::Int),
        );
        self.entries.push(Value::Record(record));
        self.matched += 1;
    }
}

fn traverse(request: &Request, root: &Path) -> BTreeMap<String, Value> {
    let mut report = Report::new(request.max_errors);
    let mut walker = WalkDir::new(root)
        // The root is the path the caller named, canonicalized by the read-path
        // policy; nothing below it is ever followed.
        .follow_links(false)
        // Keep ancestors open within the requested depth instead of buffering all their remaining names.
        .max_open(request.max_depth.saturating_add(1))
        .max_depth(request.max_depth)
        .into_iter();

    loop {
        if report.visited >= request.max_entries as i64 {
            report.stop = Some(Stop::EntryLimit);
            break;
        }
        if let Err(cancelled) = safety::check_deadline() {
            report.note_deadline(root, &cancelled);
            report.stop = Some(Stop::Deadline);
            break;
        }
        let Some(next) = walker.next() else { break };
        let entry = match next {
            Ok(entry) => entry,
            Err(error) => {
                report.visited += 1;
                report.note_error(&error, root);
                continue;
            }
        };
        report.visited += 1;
        if entry.depth() == 0 {
            continue;
        }
        if entry.file_type().is_symlink() {
            report.skipped_links += 1;
            continue;
        }
        let Some(relative) = relative_of(root, entry.path()) else {
            report.note_unrepresentable(entry.path());
            continue;
        };
        let excluded = match request.excluded(&relative) {
            Ok(value) => value,
            Err(cancelled) => {
                report.note_deadline(root, &cancelled);
                report.stop = Some(Stop::Deadline);
                break;
            }
        };
        if excluded {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }
        let included = match request.included(&relative) {
            Ok(value) => value,
            Err(cancelled) => {
                report.note_deadline(root, &cancelled);
                report.stop = Some(Stop::Deadline);
                break;
            }
        };
        if !included {
            continue;
        }
        let Some((is_dir, size, modified_ms)) = describe(&entry, request, &mut report) else {
            continue;
        };
        if let Some(threshold) = request.modified_after_ms {
            if !modified_ms.is_some_and(|milliseconds| milliseconds > threshold) {
                continue;
            }
        }
        report.push_entry(&entry, &relative, is_dir, size, modified_ms);
        if report.matched >= request.max_results as i64 {
            report.stop = Some(Stop::ResultLimit);
            break;
        }
    }

    result(request, report)
}

/// The facts an entry contributes, or nothing when it is not a kind this call
/// reports. A metadata failure is recorded and the entry is skipped.
fn describe(
    entry: &DirEntry,
    request: &Request,
    report: &mut Report,
) -> Option<(bool, i64, Option<i64>)> {
    let metadata = match entry.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            report.note_error(&error, entry.path());
            return None;
        }
    };
    let is_dir = metadata.is_dir();
    if is_dir {
        if !request.include_dirs {
            return None;
        }
    } else if !metadata.is_file() {
        // A FIFO, socket or device node is not a file this call reports, and
        // reading it is not what a listing should do.
        return None;
    }
    let size = match i64::try_from(metadata.len()) {
        Ok(size) => size,
        Err(_) => {
            report.note_coded(
                entry.path(),
                ErrorCode::Io.as_str(),
                "walk: file size is outside signed i64; no entry was returned".to_string(),
            );
            return None;
        }
    };
    let modified_ms = match metadata.modified() {
        Ok(modified) => match unix_ms(modified) {
            Some(milliseconds) => Some(milliseconds),
            None => {
                report.note_coded(
                    entry.path(),
                    ErrorCode::Io.as_str(),
                    format!(
                        "{BUILTIN}: the modification time of `{}` is outside the range this call \
                         reports, so no time is reported for it",
                        entry.path().to_string_lossy()
                    ),
                );
                None
            }
        },
        // The entry is still reported: its name and size are known, and the
        // recorded failure explains the null time.
        Err(error) => {
            report.note_io(entry.path(), &error);
            None
        }
    };
    Some((is_dir, size, modified_ms))
}

/// One entry's path relative to the root, with `/` separators, or nothing when
/// a component has no String form. Built from the components rather than by
/// replacing separators, so a backslash inside a POSIX file name stays that
/// file's own character instead of becoming a separator.
fn relative_of(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let mut segments = Vec::new();
    for component in rest.components() {
        match component {
            Component::Normal(name) => segments.push(name.to_str()?),
            _ => return None,
        }
    }
    Some(segments.join("/"))
}

/// An instant as whole milliseconds since the UTC epoch, or nothing when it
/// cannot be carried in an `Int`. A pre-epoch instant is subtracted from the
/// epoch rather than reported as unavailable: an mtime from 1969 is a fact, not
/// a failed read.
fn unix_ms(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).ok(),
        Err(before) => i64::try_from(before.duration().as_millis())
            .ok()
            .and_then(i64::checked_neg),
    }
}

/// The result record, taking the report by value so the entries and error
/// records move into it rather than being copied.
fn result(request: &Request, report: Report) -> BTreeMap<String, Value> {
    let Report {
        entries,
        errors,
        error_count,
        visited,
        matched,
        skipped_links,
        stop,
        ..
    } = report;
    let stop_reason = match stop {
        Some(Stop::Deadline) => Some("deadline"),
        Some(Stop::EntryLimit) => Some("entry_limit"),
        Some(Stop::ResultLimit) => Some("result_limit"),
        None if error_count > 0 => Some("io_errors"),
        None => None,
    };
    let errors_omitted = error_count - errors.len() as i64;
    let mut record = BTreeMap::new();
    record.insert("entries".to_string(), Value::Array(entries));
    record.insert("complete".to_string(), Value::Bool(stop_reason.is_none()));
    record.insert(
        "stop_reason".to_string(),
        stop_reason.map_or(Value::Null, |reason| Value::Str(reason.to_string())),
    );
    record.insert("visited".to_string(), Value::Int(visited));
    record.insert("matched".to_string(), Value::Int(matched));
    record.insert("errors".to_string(), Value::Array(errors));
    record.insert("errors_omitted".to_string(), Value::Int(errors_omitted));
    record.insert("error_count".to_string(), Value::Int(error_count));
    record.insert(
        "max_depth".to_string(),
        Value::Int(request.max_depth as i64),
    );
    record.insert("skipped_links".to_string(), Value::Int(skipped_links));
    record
}

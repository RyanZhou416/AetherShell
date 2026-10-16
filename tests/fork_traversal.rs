//! `walk` end to end: the option contract, the pattern language, the bounds,
//! the link and path-encoding policies, and the completeness facts.
//!
//! Every path this file touches lives in a private directory `tempfile` creates
//! exclusively for the test and removes on every path out of it, including a
//! panicking one. Nothing here writes outside that directory, changes the
//! process environment, changes the working directory, or depends on another
//! test's leftovers, so the suite is safe to run beside any other.
//!
//! `visited` and `matched` are asserted as exact numbers: they are decided by
//! the entry kinds in a fixture this file creates, so they do not depend on the
//! order a filesystem yields entries in. Which entry is *first* does depend on
//! that order, so no test asserts it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aethershell::safety;
use aethershell::traversal;
use aethershell::value::Value;
use tempfile::TempDir;

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

fn strings(items: &[&str]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| Value::Str((*item).to_string()))
            .collect(),
    )
}

fn options(pairs: &[(&str, Value)]) -> Value {
    Value::Record(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect(),
    )
}

fn call(args: Vec<Value>, input: Option<Value>) -> anyhow::Result<Value> {
    require_human_mode();
    traversal::run(args, input)
}

fn walk(path: &Path) -> BTreeMap<String, Value> {
    match call(vec![as_value(path)], None) {
        Ok(value) => record(value),
        Err(error) => panic!("walk({}) failed: {error}", path.display()),
    }
}

fn walk_with(path: &Path, options: Value) -> BTreeMap<String, Value> {
    match call(vec![as_value(path), options], None) {
        Ok(value) => record(value),
        Err(error) => panic!("walk({}, options) failed: {error}", path.display()),
    }
}

/// The message of a refusal, which carries the code the caller branches on.
fn refusal(args: Vec<Value>, input: Option<Value>) -> String {
    match call(args, input) {
        Err(error) => error.to_string(),
        Ok(value) => panic!("walk accepted a call it must refuse: {value:?}"),
    }
}

// ── reading the result ──────────────────────────────────────────────────

fn record(value: Value) -> BTreeMap<String, Value> {
    match value {
        Value::Record(record) => record,
        other => panic!("walk answered {other:?} instead of a result record"),
    }
}

fn field<'a>(result: &'a BTreeMap<String, Value>, key: &str) -> &'a Value {
    result
        .get(key)
        .unwrap_or_else(|| panic!("`{key}` is missing from {result:?}"))
}

fn int(result: &BTreeMap<String, Value>, key: &str) -> i64 {
    match field(result, key) {
        Value::Int(number) => *number,
        other => panic!("`{key}` is {other:?}, not an Int"),
    }
}

fn flag(result: &BTreeMap<String, Value>, key: &str) -> bool {
    match field(result, key) {
        Value::Bool(flag) => *flag,
        other => panic!("`{key}` is {other:?}, not a Bool"),
    }
}

fn optional_int(result: &BTreeMap<String, Value>, key: &str) -> Option<i64> {
    match field(result, key) {
        Value::Null => None,
        Value::Int(number) => Some(*number),
        other => panic!("`{key}` is {other:?}, not an Int or null"),
    }
}

fn optional_text(result: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    match field(result, key) {
        Value::Null => None,
        Value::Str(text) => Some(text.clone()),
        other => panic!("`{key}` is {other:?}, not a String or null"),
    }
}

fn text(result: &BTreeMap<String, Value>, key: &str) -> String {
    match field(result, key) {
        Value::Str(text) => text.clone(),
        other => panic!("`{key}` is {other:?}, not a String"),
    }
}

fn items<'a>(result: &'a BTreeMap<String, Value>, key: &str) -> &'a [Value] {
    match field(result, key) {
        Value::Array(items) => items.as_slice(),
        other => panic!("`{key}` is {other:?}, not an Array"),
    }
}

fn nested(result: &BTreeMap<String, Value>, key: &str) -> Vec<BTreeMap<String, Value>> {
    items(result, key)
        .iter()
        .map(|item| match item {
            Value::Record(record) => record.clone(),
            other => panic!("an element of `{key}` is {other:?}, not a Record"),
        })
        .collect()
}

/// The reported relative paths, sorted — what a caller does with a completed
/// result.
fn relative_paths(result: &BTreeMap<String, Value>) -> Vec<String> {
    let mut paths: Vec<String> = nested(result, "entries")
        .iter()
        .map(|entry| text(entry, "relative_path"))
        .collect();
    paths.sort();
    paths
}

// ── fixtures ────────────────────────────────────────────────────────────

fn write(root: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", parent.display()));
    }
    std::fs::write(&path, contents)
        .unwrap_or_else(|error| panic!("cannot write {}: {error}", path.display()));
    path
}

/// Eleven files in four directories, one of them hidden, one nested twice.
fn standard_tree() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("private fixture directory");
    let root = directory.path().join("standard");
    for (relative, contents) in [
        ("root.rs", "fn main() {}\n"),
        ("top.txt", "top\n"),
        ("notes.md", "notes\n"),
        (".hidden.txt", "hidden\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/lib.rs", "pub fn lib() {}\n"),
        ("src/nested/deep.rs", "pub fn deep() {}\n"),
        ("src/nested/skip.tmp", "tmp\n"),
        ("build/artifact.rs", "pub fn built() {}\n"),
        ("data/a.txt", "a\n"),
        ("data/b.txt", "b\n"),
    ] {
        write(&root, relative, contents);
    }
    (directory, root)
}

/// Five files in two directories, so a pruned subtree and a stopped walk are
/// both small enough to count by hand.
fn limits_tree() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("private fixture directory");
    let root = directory.path().join("limits");
    for relative in [
        "keep1.txt",
        "keep2.txt",
        "prune/a.txt",
        "prune/b.txt",
        "prune/deeper/c.txt",
    ] {
        write(&root, relative, "x\n");
    }
    (directory, root)
}

/// The eleven files of [`standard_tree`], as sorted relative paths.
const STANDARD_FILES: [&str; 11] = [
    ".hidden.txt",
    "build/artifact.rs",
    "data/a.txt",
    "data/b.txt",
    "notes.md",
    "root.rs",
    "src/lib.rs",
    "src/main.rs",
    "src/nested/deep.rs",
    "src/nested/skip.tmp",
    "top.txt",
];

// ── the frozen records ──────────────────────────────────────────────────

#[test]
fn the_result_and_entry_records_carry_exactly_the_frozen_fields() {
    let (_directory, root) = standard_tree();
    let result = walk(&root);

    let keys: Vec<&str> = result.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "complete",
            "entries",
            "error_count",
            "errors",
            "errors_omitted",
            "matched",
            "max_depth",
            "skipped_links",
            "stop_reason",
            "visited",
        ]
    );

    let entries = nested(&result, "entries");
    assert_eq!(entries.len(), 11, "every file, and no directory");
    let entry_keys: Vec<&str> = entries[0].keys().map(String::as_str).collect();
    assert_eq!(
        entry_keys,
        [
            "is_dir",
            "modified_ms",
            "name",
            "path",
            "relative_path",
            "size",
        ]
    );

    assert_eq!(flag(&result, "complete"), true);
    assert_eq!(optional_text(&result, "stop_reason"), None);
    assert_eq!(int(&result, "error_count"), 0);
    assert_eq!(int(&result, "errors_omitted"), 0);
    assert_eq!(nested(&result, "errors").len(), 0);
    assert_eq!(int(&result, "max_depth"), 64, "the documented default");
    assert_eq!(int(&result, "skipped_links"), 0);
    assert_eq!(int(&result, "matched"), 11);
    assert_eq!(int(&result, "matched"), entries.len() as i64);
    // The root plus eleven files plus src, src/nested, build and data.
    assert_eq!(int(&result, "visited"), 16);
    assert_eq!(relative_paths(&result), STANDARD_FILES);
}

#[test]
fn the_root_is_never_an_entry_and_depth_zero_visits_only_the_root() {
    let (_directory, root) = standard_tree();

    let zero = walk_with(&root, options(&[("max_depth", Value::Int(0))]));
    assert_eq!(nested(&zero, "entries").len(), 0, "the root is the scope");
    assert_eq!(int(&zero, "visited"), 1);
    assert_eq!(int(&zero, "matched"), 0);
    assert_eq!(flag(&zero, "complete"), true);
    assert_eq!(optional_text(&zero, "stop_reason"), None);
    assert_eq!(int(&zero, "max_depth"), 0, "the value used is echoed");

    let one = walk_with(&root, options(&[("max_depth", Value::Int(1))]));
    assert_eq!(
        relative_paths(&one),
        [".hidden.txt", "notes.md", "root.rs", "top.txt"],
        "one level below the root: four files, and the three directories there are visited but not reported"
    );
    assert_eq!(int(&one, "visited"), 8);
    assert_eq!(int(&one, "matched"), 4);
    assert_eq!(flag(&one, "complete"), true);
}

#[test]
fn files_are_listed_by_default_and_directories_only_when_asked() {
    let (_directory, root) = standard_tree();

    let files = walk(&root);
    let reported = nested(&files, "entries");
    assert!(
        reported.iter().all(|entry| !flag(entry, "is_dir")),
        "no directory is an entry by default"
    );

    let both = walk_with(&root, options(&[("include_dirs", Value::Bool(true))]));
    assert_eq!(
        int(&both, "matched"),
        15,
        "eleven files and four directories"
    );
    assert_eq!(int(&both, "visited"), 16, "the same traversal either way");
    let directories: Vec<String> = nested(&both, "entries")
        .iter()
        .filter(|entry| flag(entry, "is_dir"))
        .map(|entry| text(entry, "relative_path"))
        .collect();
    let mut directories = directories;
    directories.sort();
    assert_eq!(
        directories,
        ["build", "data", "src", "src/nested"],
        "a directory entry is a directory, never the root"
    );
}

// ── patterns ────────────────────────────────────────────────────────────

fn included(root: &Path, patterns: &[&str]) -> Vec<String> {
    relative_paths(&walk_with(root, options(&[("include", strings(patterns))])))
}

#[test]
fn include_patterns_select_by_relative_path() {
    let (_directory, root) = standard_tree();

    assert_eq!(
        included(&root, &["**/*.rs"]),
        [
            "build/artifact.rs",
            "root.rs",
            "src/lib.rs",
            "src/main.rs",
            "src/nested/deep.rs",
        ],
        "`**` matches zero segments, so a root-level file is included"
    );
    assert_eq!(
        included(&root, &["*.txt"]),
        [".hidden.txt", "top.txt"],
        "one segment only, and a leading dot is an ordinary character"
    );
    assert_eq!(
        included(&root, &["src/*.rs"]),
        ["src/lib.rs", "src/main.rs"]
    );
    assert_eq!(included(&root, &["**"]), STANDARD_FILES);
    assert_eq!(
        included(&root, &["src/**/*.rs"]),
        ["src/lib.rs", "src/main.rs", "src/nested/deep.rs"],
        "`**` matches zero segments below `src`, and a pattern is anchored at the root"
    );
}

#[test]
fn globstar_and_segment_wildcards_are_honoured() {
    let (_directory, root) = standard_tree();

    assert_eq!(included(&root, &["?op.txt"]), ["top.txt"]);
    assert_eq!(
        included(&root, &["data/[ab].txt"]),
        ["data/a.txt", "data/b.txt"]
    );
    assert_eq!(included(&root, &["data/[!a].txt"]), ["data/b.txt"]);
    assert_eq!(
        included(&root, &["**/nested/*.rs"]),
        ["src/nested/deep.rs"],
        "`**` spans any depth"
    );
    assert_eq!(
        included(&root, &["src/**"]),
        [
            "src/lib.rs",
            "src/main.rs",
            "src/nested/deep.rs",
            "src/nested/skip.tmp",
        ],
        "`**` stands alone in its segment and covers everything below"
    );
}

#[test]
fn exclude_prunes_a_directory_subtree_before_descending() {
    let (_directory, root) = limits_tree();

    let all = walk(&root);
    assert_eq!(int(&all, "visited"), 8, "root, five files, two directories");
    assert_eq!(int(&all, "matched"), 5);
    assert_eq!(flag(&all, "complete"), true);

    let pruned = walk_with(&root, options(&[("exclude", strings(&["prune"]))]));
    assert_eq!(
        relative_paths(&pruned),
        ["keep1.txt", "keep2.txt"],
        "the pruned subtree contributes nothing"
    );
    assert_eq!(
        int(&pruned, "visited"),
        4,
        "the excluded directory counts as a visit, but none of its descendants do"
    );
    assert_eq!(int(&pruned, "matched"), 2);
    assert_eq!(
        flag(&pruned, "complete"),
        true,
        "a requested exclusion is not an incomplete walk"
    );
    assert_eq!(optional_text(&pruned, "stop_reason"), None);
}

#[test]
fn an_exclude_pattern_that_matches_a_directory_prunes_it() {
    let (_directory, root) = standard_tree();

    let pruned = walk_with(&root, options(&[("exclude", strings(&["src/nested/**"]))]));
    assert_eq!(
        int(&pruned, "visited"),
        14,
        "the excluded directory is visited but its two files are not"
    );
    assert_eq!(int(&pruned, "matched"), 9);
    assert!(
        relative_paths(&pruned)
            .iter()
            .all(|path| !path.starts_with("src/nested")),
        "nothing under the pruned directory is reported"
    );
}

#[test]
fn excluded_directories_still_obey_the_visit_bound() {
    let directory = tempfile::tempdir().unwrap();
    for number in 0..100 {
        std::fs::create_dir(directory.path().join(format!("skip-{number}"))).unwrap();
    }
    let result = walk_with(
        directory.path(),
        options(&[
            ("exclude", strings(&["skip-*"])),
            ("max_entries", Value::Int(3)),
        ]),
    );
    assert_eq!(int(&result, "visited"), 3);
    assert_eq!(int(&result, "matched"), 0);
    assert!(!flag(&result, "complete"));
    assert_eq!(
        optional_text(&result, "stop_reason").as_deref(),
        Some("entry_limit")
    );
}

#[test]
fn pattern_matching_observes_an_expired_thread_deadline() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("file.txt"), b"data").unwrap();
    let pattern = vec!["segment"; 500].join("/");
    let patterns = Value::Array(vec![
        Value::Str(pattern.clone()),
        Value::Str(pattern.clone()),
        Value::Str(pattern),
    ]);
    let _deadline = aethershell::safety::enter_deadline(std::time::Duration::ZERO);
    let result = walk_with(directory.path(), options(&[("include", patterns)]));
    assert!(!flag(&result, "complete"));
    assert_eq!(
        optional_text(&result, "stop_reason").as_deref(),
        Some("deadline")
    );
    assert_eq!(int(&result, "matched"), 0);
}

#[test]
fn a_malformed_pattern_is_refused_before_the_filesystem_is_touched() {
    let directory = tempfile::tempdir().expect("private fixture directory");
    let missing = directory.path().join("absent");
    assert!(!missing.exists());

    let long = "a".repeat(4_097);
    let cases: [(&str, &str); 11] = [
        ("", "an empty pattern"),
        ("/etc/*", "an absolute pattern"),
        ("C:/*.rs", "a drive prefix"),
        ("src\\*.rs", "a backslash"),
        ("a/../b", "a `..` segment"),
        ("./a", "a `.` segment"),
        ("a//b", "a doubled separator"),
        ("src/", "a trailing separator"),
        ("[abc", "an unterminated class"),
        ("[]", "an empty class"),
        ("[z-a]", "a backwards range"),
    ];
    for (pattern, why) in cases {
        for key in ["include", "exclude"] {
            let message = refusal(
                vec![as_value(&missing), options(&[(key, strings(&[pattern]))])],
                None,
            );
            assert!(
                message.contains("E_BAD_ARG"),
                "{why} ({pattern:?}) is not refused: {message}"
            );
            assert!(
                !message.contains("E_NOT_FOUND"),
                "{why} ({pattern:?}) reached the filesystem: {message}"
            );
        }
    }

    let message = refusal(
        vec![
            as_value(&missing),
            options(&[("include", strings(&[long.as_str()]))]),
        ],
        None,
    );
    assert!(
        message.contains("E_BAD_ARG"),
        "an over-long pattern is refused: {message}"
    );

    // A pattern that is well formed is not refused, so the checks above prove
    // the pattern and not the missing path.
    let message = refusal(
        vec![
            as_value(&missing),
            options(&[("include", strings(&["src/**/*.rs"]))]),
        ],
        None,
    );
    assert!(
        message.contains("E_NOT_FOUND"),
        "a usable pattern reaches the path check: {message}"
    );
}

// ── arguments ───────────────────────────────────────────────────────────

#[test]
fn options_are_validated_before_the_filesystem_is_touched() {
    let directory = tempfile::tempdir().expect("private fixture directory");
    let missing = directory.path().join("absent");

    let cases: Vec<(&str, Value)> = vec![
        (
            "max_depth below zero",
            options(&[("max_depth", Value::Int(-1))]),
        ),
        (
            "max_depth past the cap",
            options(&[("max_depth", Value::Int(4_097))]),
        ),
        (
            "max_depth of another type",
            options(&[("max_depth", Value::Str("2".into()))]),
        ),
        (
            "max_results of zero",
            options(&[("max_results", Value::Int(0))]),
        ),
        (
            "max_results past the cap",
            options(&[("max_results", Value::Int(1_000_001))]),
        ),
        (
            "max_entries of zero",
            options(&[("max_entries", Value::Int(0))]),
        ),
        (
            "max_entries past the cap",
            options(&[("max_entries", Value::Int(10_000_001))]),
        ),
        (
            "max_errors of zero",
            options(&[("max_errors", Value::Int(0))]),
        ),
        (
            "max_errors past the cap",
            options(&[("max_errors", Value::Int(1_001))]),
        ),
        (
            "a negative modified_after_ms",
            options(&[("modified_after_ms", Value::Int(-1))]),
        ),
        (
            "modified_after_ms of another type",
            options(&[("modified_after_ms", Value::Str("now".into()))]),
        ),
        (
            "include that is not an Array",
            options(&[("include", Value::Str("*.rs".into()))]),
        ),
        (
            "an include element that is not a String",
            options(&[("include", Value::Array(vec![Value::Int(1)]))]),
        ),
        (
            "exclude that is not an Array",
            options(&[("exclude", Value::Int(1))]),
        ),
        (
            "include_dirs that is not a Bool",
            options(&[("include_dirs", Value::Str("yes".into()))]),
        ),
        (
            "an unknown option key",
            options(&[("pattern", Value::Str("*.rs".into()))]),
        ),
        (
            "a case-mismatched key",
            options(&[("maxDepth", Value::Int(1))]),
        ),
    ];
    for (why, option) in cases {
        let message = refusal(vec![as_value(&missing), option], None);
        assert!(
            message.contains("E_BAD_ARG"),
            "{why} is not refused: {message}"
        );
        assert!(
            !message.contains("E_NOT_FOUND"),
            "{why} reached the filesystem: {message}"
        );
    }

    // Argument shapes are refused the same way.
    let shapes: Vec<(&str, Vec<Value>, Option<Value>)> = vec![
        ("no argument at all", vec![], None),
        ("a path that is not a String", vec![Value::Int(1)], None),
        (
            "options that are not a Record",
            vec![as_value(&missing), Value::Int(1)],
            None,
        ),
        (
            "a third argument",
            vec![as_value(&missing), options(&[]), options(&[])],
            None,
        ),
        (
            "a piped subject",
            vec![as_value(&missing)],
            Some(Value::Str("piped".into())),
        ),
    ];
    for (why, args, input) in shapes {
        let message = refusal(args, input);
        assert!(
            message.contains("E_BAD_ARG"),
            "{why} is not refused: {message}"
        );
    }
}

#[test]
fn a_missing_root_is_e_not_found_and_a_file_root_is_e_bad_arg() {
    let directory = tempfile::tempdir().expect("private fixture directory");
    let missing = directory.path().join("absent");
    let message = refusal(vec![as_value(&missing)], None);
    assert!(message.contains("E_NOT_FOUND"), "{message}");

    let file = write(directory.path(), "one.txt", "x\n");
    let message = refusal(vec![as_value(&file)], None);
    assert!(
        message.contains("E_BAD_ARG") && !message.contains("E_NOT_FOUND"),
        "a regular file is a mis-specified scope, not a missing directory: {message}"
    );

    // The two refusal kinds are told apart, so neither assertion is vacuous.
    let absent = refusal(vec![as_value(&missing)], None);
    assert!(!absent.contains("E_BAD_ARG"), "{absent}");
}

// ── bounds ──────────────────────────────────────────────────────────────

#[test]
fn max_results_stops_the_walk_at_the_limit() {
    let (_directory, root) = limits_tree();

    let one = walk_with(&root, options(&[("max_results", Value::Int(1))]));
    assert_eq!(int(&one, "matched"), 1);
    assert_eq!(nested(&one, "entries").len(), 1);
    assert_eq!(flag(&one, "complete"), false);
    assert_eq!(
        optional_text(&one, "stop_reason").as_deref(),
        Some("result_limit")
    );

    let two = walk_with(&root, options(&[("max_results", Value::Int(2))]));
    assert_eq!(int(&two, "matched"), 2);
    assert_eq!(flag(&two, "complete"), false);
    assert_eq!(
        optional_text(&two, "stop_reason").as_deref(),
        Some("result_limit")
    );

    // A limit the tree cannot reach is not a stop.
    let enough = walk_with(&root, options(&[("max_results", Value::Int(99))]));
    assert_eq!(flag(&enough, "complete"), true);
    assert_eq!(optional_text(&enough, "stop_reason"), None);
    assert_eq!(int(&enough, "matched"), 5);
}

#[test]
fn max_entries_bounds_the_visit_before_the_results() {
    let (_directory, root) = limits_tree();

    let root_only = walk_with(&root, options(&[("max_entries", Value::Int(1))]));
    assert_eq!(int(&root_only, "visited"), 1, "the root is the first entry");
    assert_eq!(int(&root_only, "matched"), 0);
    assert_eq!(nested(&root_only, "entries").len(), 0);
    assert_eq!(flag(&root_only, "complete"), false);
    assert_eq!(
        optional_text(&root_only, "stop_reason").as_deref(),
        Some("entry_limit")
    );

    let three = walk_with(&root, options(&[("max_entries", Value::Int(3))]));
    assert_eq!(int(&three, "visited"), 3);
    assert!(int(&three, "matched") <= 3);
    assert_eq!(
        int(&three, "matched"),
        nested(&three, "entries").len() as i64
    );
    assert_eq!(flag(&three, "complete"), false);
    assert_eq!(
        optional_text(&three, "stop_reason").as_deref(),
        Some("entry_limit")
    );
}

// ── modification times ──────────────────────────────────────────────────

/// Set a file's modification time, and answer what was asked for, in whole
/// milliseconds since the UTC epoch.
fn set_modified(path: &Path, seconds_after_epoch: u64) -> i64 {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds_after_epoch);
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|error| panic!("cannot open {}: {error}", path.display()))
        .set_modified(time)
        .unwrap_or_else(|error| panic!("cannot set the time on {}: {error}", path.display()));
    (seconds_after_epoch * 1_000) as i64
}

#[test]
fn modified_after_ms_selects_by_time_and_reports_milliseconds() {
    let directory = tempfile::tempdir().expect("private fixture directory");
    let root = directory.path().join("times");
    // Hours apart, so a filesystem that rounds a stored time cannot reorder
    // them or move one across the threshold below.
    let old = set_modified(&write(&root, "old.txt", "old\n"), 1_000_000);
    let new = set_modified(&write(&root, "new.txt", "new\n"), 1_003_600);
    let latest = set_modified(&write(&root, "latest.txt", "latest\n"), 1_007_200);
    assert!(old < new && new < latest);

    let all = walk(&root);
    assert_eq!(relative_paths(&all), ["latest.txt", "new.txt", "old.txt"]);
    let reported = |result: &BTreeMap<String, Value>, name: &str| -> Option<i64> {
        nested(result, "entries")
            .iter()
            .find(|entry| text(entry, "name") == name)
            .and_then(|entry| optional_int(entry, "modified_ms"))
    };
    let asked = (1_003_600u64 * 1_000) as i64;
    let reported_new = reported(&all, "new.txt").expect("a reportable time");
    assert!(
        (reported_new - asked).abs() < 5_000,
        "the report is milliseconds since the UTC epoch: asked {asked}, observed {reported_new}"
    );

    let threshold = (new + old) / 2;
    let recent = walk_with(
        &root,
        options(&[("modified_after_ms", Value::Int(threshold))]),
    );
    assert_eq!(relative_paths(&recent), ["latest.txt", "new.txt"]);
    assert_eq!(flag(&recent, "complete"), true);

    let everything = walk_with(&root, options(&[("modified_after_ms", Value::Int(0))]));
    assert_eq!(
        relative_paths(&everything),
        ["latest.txt", "new.txt", "old.txt"]
    );

    // "After" is strict: the threshold set to an entry's own reported time
    // excludes that entry, and one millisecond earlier includes only it. The
    // reported time is used rather than the requested one, so a filesystem that
    // stores a coarser timestamp cannot make this assertion depend on rounding.
    let margin = walk_with(
        &root,
        options(&[("modified_after_ms", Value::Int(reported_new))]),
    );
    assert_eq!(
        relative_paths(&margin),
        ["latest.txt"],
        "an entry at exactly the threshold is not after it"
    );
    let boundary = walk_with(
        &root,
        options(&[("modified_after_ms", Value::Int(reported_new - 1))]),
    );
    assert_eq!(relative_paths(&boundary), ["latest.txt", "new.txt"]);
}

// ── entry kinds, links and encodings ────────────────────────────────────

#[cfg(unix)]
#[test]
fn a_non_regular_file_is_skipped_rather_than_reported() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let directory = tempfile::tempdir().expect("private fixture directory");
    let root = directory.path().join("kinds");
    write(&root, "regular.txt", "x\n");
    let fifo = root.join("pipe");
    let name = CString::new(fifo.as_os_str().as_bytes()).expect("a path without a NUL");
    // SAFETY: `name` is a live NUL-terminated path and the mode is a literal.
    let made = unsafe { libc::mkfifo(name.as_ptr(), 0o644) };
    assert_eq!(
        made,
        0,
        "mkfifo failed: {}",
        std::io::Error::last_os_error()
    );

    let result = walk(&root);
    assert_eq!(relative_paths(&result), ["regular.txt"]);
    assert_eq!(int(&result, "matched"), 1);
    assert_eq!(int(&result, "error_count"), 0, "not a file, not a failure");
    assert_eq!(flag(&result, "complete"), true);
    assert_eq!(int(&result, "skipped_links"), 0);
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_never_followed_and_are_counted() {
    let directory = tempfile::tempdir().expect("private fixture directory");
    let root = directory.path().join("root");
    let outside = directory.path().join("outside");
    write(&root, "inside.txt", "inside\n");
    write(&outside, "escape.txt", "outside\n");
    std::os::unix::fs::symlink(&outside, root.join("escape")).expect("a directory link");
    std::os::unix::fs::symlink(root.join("inside.txt"), root.join("alias.txt"))
        .expect("a file link");

    let result = walk(&root);
    assert_eq!(
        relative_paths(&result),
        ["inside.txt"],
        "a link is not an entry, and the link's target is not walked through"
    );
    assert_eq!(int(&result, "skipped_links"), 2);
    assert_eq!(int(&result, "visited"), 4, "root, one file, two links");
    assert_eq!(int(&result, "matched"), 1);
    assert_eq!(int(&result, "error_count"), 0);
    assert_eq!(flag(&result, "complete"), true);
    assert!(
        nested(&result, "entries")
            .iter()
            .all(|entry| !text(entry, "path").contains("escape")),
        "the link's target is outside the walk root"
    );
    let with_dirs = walk_with(&root, options(&[("include_dirs", Value::Bool(true))]));
    assert_eq!(
        relative_paths(&with_dirs),
        ["inside.txt"],
        "a directory link is not a directory entry either"
    );
}

#[cfg(unix)]
#[test]
fn a_name_that_is_not_utf8_is_reported_and_never_substituted() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let directory = tempfile::tempdir().expect("private fixture directory");
    let root = directory.path().join("names");
    write(&root, "ok.txt", "ok\n");
    write(&root, "bad\u{fffd}.txt", "clash\n");
    std::fs::write(root.join(OsStr::from_bytes(b"bad\xff.txt")), "bad\n")
        .expect("a non-UTF-8 name");

    let result = walk(&root);
    assert_eq!(
        relative_paths(&result),
        ["bad\u{fffd}.txt", "ok.txt"],
        "the real replacement character is data; the invalid name is not rendered as one"
    );
    assert_eq!(int(&result, "matched"), 2);
    assert_eq!(int(&result, "error_count"), 1);
    assert_eq!(int(&result, "errors_omitted"), 0);
    assert_eq!(flag(&result, "complete"), false);
    assert_eq!(
        optional_text(&result, "stop_reason").as_deref(),
        Some("io_errors")
    );
    let errors = nested(&result, "errors");
    assert_eq!(errors.len(), 1);
    assert_eq!(text(&errors[0], "code"), "E_IO");
    assert!(
        text(&errors[0], "message").contains("UTF-8"),
        "the failure says what is wrong: {}",
        text(&errors[0], "message")
    );

    // A second unreportable name is counted even when the kept records are
    // exhausted: `error_count` is the honest total.
    std::fs::write(root.join(OsStr::from_bytes(b"worse\xfe.txt")), "worse\n")
        .expect("a second non-UTF-8 name");
    let bounded = walk_with(&root, options(&[("max_errors", Value::Int(1))]));
    assert_eq!(int(&bounded, "error_count"), 2);
    assert_eq!(nested(&bounded, "errors").len(), 1);
    assert_eq!(int(&bounded, "errors_omitted"), 1);
    assert_eq!(int(&bounded, "matched"), 2);
    assert_eq!(flag(&bounded, "complete"), false);
}

#[cfg(windows)]
#[test]
fn windows_directory_junctions_are_not_entered() {
    struct Junction(std::path::PathBuf);
    impl Drop for Junction {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    let root = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("local.txt"), b"local").unwrap();
    std::fs::write(target.path().join("outside.txt"), b"outside").unwrap();
    let link = root.path().join("junction");
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(target.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _cleanup = Junction(link);
    let result = walk(root.path());
    assert_eq!(relative_paths(&result), ["local.txt"]);
    assert_eq!(int(&result, "skipped_links"), 1);
    assert!(flag(&result, "complete"));
}

#[test]
fn relative_path_and_name_describe_the_same_entry() {
    let (_directory, root) = standard_tree();
    let result = walk(&root);

    for entry in nested(&result, "entries") {
        let relative = text(&entry, "relative_path");
        let name = text(&entry, "name");
        assert!(!relative.contains('\\'), "{relative} uses a separator");
        assert!(!relative.starts_with('/'), "{relative} is not relative");
        assert_eq!(
            relative.rsplit('/').next(),
            Some(name.as_str()),
            "the name is the last segment of the relative path"
        );
        assert!(
            text(&entry, "path").contains(&name),
            "the path names the entry too"
        );
        assert_eq!(flag(&entry, "is_dir"), false);
        // The reported size is the size on disk, and the relative path is
        // resolvable under the root with its `/` separators on every platform.
        let on_disk = std::fs::metadata(root.join(&relative))
            .unwrap_or_else(|error| panic!("cannot stat {relative}: {error}"));
        assert_eq!(int(&entry, "size"), on_disk.len() as i64, "{relative}");
        assert!(
            optional_int(&entry, "modified_ms").is_some(),
            "{relative} has a readable modification time here"
        );
    }
}

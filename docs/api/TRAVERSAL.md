# Bounded directory traversal

`walk(path, options?)` lists the entries below one directory in a single bounded pass and returns one record that states what the traversal did. Use it when a listing must be bounded, filtered by glob, filtered by modification time, or when the caller has to tell "the tree ended" apart from "the walk stopped at a limit". `find(path, pattern?)`, `fs_walk(path)` and `tree(path)` keep their existing result types and traversal behavior; `walk` replaces none of them.

Every argument decision is made before the filesystem is read: the options record is validated, every pattern is compiled, and only then is the root resolved through the read-path policy `file_read_text` also uses. A malformed call is therefore `E_BAD_ARG` even when the path does not exist. A path that does not exist is `E_NOT_FOUND` and a path that is not a directory is `E_BAD_ARG`. The root is the scope, not an entry of its own listing, so `max_depth: 0` visits the root and reports nothing below it.

## Options

| Option | Default | Values |
|---|---|---|
| `include` | every file | Array of at most 1024 glob patterns, each at most 4096 bytes; an entry is reported when any pattern matches |
| `exclude` | none | Same form; a matching entry is dropped, and a matching directory is not descended into |
| `max_depth` | `64` | Integer `0..4096`; levels below the root |
| `max_results` | `1000` | Integer `1..1000000`; reported entries |
| `max_entries` | `100000` | Integer `1..10000000`; entries visited, reported or not |
| `max_errors` | `32` | Integer `1..1000`; kept error records, while `error_count` still counts every failure |
| `include_dirs` | `false` | Bool; directories are reported as entries too |
| `modified_after_ms` | absent | Integer `>= 0`; UTC epoch milliseconds an entry must be modified strictly after |

An unknown option key, a wrong type and an out-of-range value are all `E_BAD_ARG`. Nothing is guessed from a near-miss key and no unknown key is ignored. A piped subject is refused: the path is the call.

```ae
walk("./src")
walk("./src", {include: ["**/*.rs", "**/*.toml"], exclude: ["target/**"]})
walk("./src", {include_dirs: true, max_depth: 1})
walk("./src", {modified_after_ms: 1767225600000, max_results: 50})
```

## Patterns

- A pattern is matched against the entry's path **relative to the walk root**, with `/` separators on every platform, and is anchored at the root: `src/*.rs` never matches `a/src/b.rs`.
- `*` and `?` match inside one path segment and never cross `/`. `[abc]`, `[a-z]` and `[!abc]` (or `[^abc]`) match one character. A range must run from the lower to the higher character.
- `**` alone in a segment matches zero or more whole segments, so `**/*.rs` matches both `top.rs` and `src/nested/deep.rs`, and `src/nested/**` matches the `src/nested` directory itself. A `**` beside other characters in one segment matches like `*`.
- A leading `.` is an ordinary character: `*.txt` matches `.hidden.txt`. Nothing is hidden from a pattern.
- A backslash is refused rather than read as an escape, because on Windows it is the separator a caller probably wrote. Absolute patterns, drive prefixes, `.` and `..` segments, empty segments, an unterminated `[`, an empty class and an over-long pattern are `E_BAD_ARG`.
- Matching is byte-for-byte against the reported `relative_path`. On a case-insensitive filesystem a pattern must use the spelling the filesystem reports.

`include` and `exclude` are evaluated against every entry, directories included. `exclude` wins, and a directory it matches is pruned before it is descended into, so a pattern such as `target/**` also drops the matching directory entry itself. `include` never prunes: a directory that does not match may still hold files that do.

## Result

| Field | Meaning |
|---|---|
| `entries` | The reported entries, each `{path, relative_path, name, is_dir, size, modified_ms}` |
| `complete` | True only when the walk reached the end of the tree with no limit stop and no recorded failure |
| `stop_reason` | `"result_limit"`, `"entry_limit"`, `"deadline"`, `"io_errors"`, or null |
| `visited` | The root plus every entry the walk yielded |
| `matched` | Reported entries; equal to the length of `entries` |
| `errors` | At most `max_errors` records `{path, code, message}` |
| `errors_omitted` | Failures counted but not kept as records |
| `error_count` | Every failure, kept or omitted |
| `max_depth` | The depth bound this walk used |
| `skipped_links` | Symbolic links met and not followed |

```ae
let listing = walk("./src", {include: ["**/*.rs"]})
# `complete` says whether the entries are the whole tree; `stop_reason` says why not.
listing.complete
listing.stop_reason
listing.entries | map(fn(e) => e.relative_path)
```

`complete` is false when the walk stopped at a limit and when any failure was recorded, because in both cases the listing is not the whole tree. `stop_reason` names the single reason the walk ended: a stop reached during the walk (`deadline`, `entry_limit`, `result_limit`), otherwise `io_errors` when at least one failure was recorded, otherwise null. `include`, `exclude` and `max_depth` are requests rather than failures: a walk that honours them and finishes is complete. Hitting a visit or result limit is conservatively reported as incomplete even if that limit happened to land on the tree's last entry; the walk does not scan ahead to prove exhaustion.

`visited` counts the root and every entry the walk yielded. A pruned directory itself is counted but its descendants contribute nothing; every entry dropped by `exclude` is counted, and an unreadable directory that produced a failure is counted as the entry the walk could not read. `matched` is what was reported, so `matched <= visited` always holds and `matched == entries.len()`.

## Entries, paths and times

- Only regular files are reported, plus directories when `include_dirs` is true. Symbolic links below the root are never followed or reported; each is counted in `skipped_links`. The root itself is resolved by the read-path policy, which may refuse links; so a link cycle cannot be walked and a link out of the tree cannot be entered through. FIFOs, sockets and device nodes are skipped without an error record.
- `path` is the entry as the walk named it under the root, `relative_path` is that path relative to the root with `/` separators, and `name` is the last segment of `relative_path`.
- A name that is not valid UTF-8 is not reported as an entry at all: a lossy rendering could collide with a real name that exists. It is recorded in `errors` with code `E_IO`, and `complete` is false. A real U+FFFD in a name is ordinary data and is reported as it is.
- `size` is the length in bytes; for a directory it is whatever the platform reports.
- `modified_ms` is whole milliseconds since the UTC epoch, negative before 1970, and null when the platform cannot report the time — in which case a failure is recorded and `complete` is false. The value is truncated to whole milliseconds rather than rounded, so it is never presented as more precise than the clock.
- `modified_after_ms` is strict. An entry whose time equals the threshold is not reported, and an entry with no reportable time is not reported while this option is set.

## Order, memory and time

Entries arrive in the order the filesystem yields them. That order is not specified, is not sorted, and differs between hosts and even between two walks of one directory. It is deliberately not sorted: ordering one directory is exactly the step that must hold every name in memory at once, which a bounded listing must not do. A caller that needs an order sorts a completed result itself, and `max_results` bounds how much there is to sort.

The traversal does not sort or pre-collect directory listings. It permits one open directory handle per active depth instead of buffering the remaining names of an ancestor; `max_depth` bounds this handle budget, and lower operating-system limits can cause reported I/O errors. Result entries and retained errors have separate explicit caps. `max_entries` bounds the work even when nothing matches, so a huge tree with a narrow `include` costs the visit bound rather than the result bound.

The evaluator's deadline is checked before each entry and during pattern matching. Checks are sampled; this is not a hard wall-clock bound on a blocked filesystem call. When an in-process deadline is observed, entries found so far are returned with an `E_BUDGET_EXCEEDED` record and `stop_reason: "deadline"`. A DSH process deadline may instead terminate the interpreter, leaving no partial result.

## Failures inside the tree

A directory that cannot be read, a file whose metadata cannot be read and a name that cannot be reported are recorded, and the walk continues. Each record carries the same code the rest of the shell uses for that condition: `E_NOT_FOUND`, `E_POLICY_DENY` or `E_IO`. A refusal, by contrast, is only ever about the call or the root, and is raised before the walk starts.

## Legacy traversal is unchanged

- `find(path, pattern?)` still returns an `Array` of paths and still walks as it did, with no visit bound, no error records and no completeness fact. It is the right call when a caller wants only the paths.
- `fs_walk(path)` and `fs.walk` still return an `Array` of every path below a directory, recursively, with no bounds, no filtering and no failure reporting.
- `tree(path)` still renders a text tree, and `ls(path)` still lists one directory level.
- `file_read_text` remains the bounded single-file read, and `walk` neither reads file contents nor reports them.

# Explicit process output, environment, input and encoding

`proc_run(argv, options?)`, also available as `proc.run`, runs a foreground program directly. `argv` is a non-empty array of strings: the first item names the executable and each remaining item is one literal argument. There is no implicit shell, variable expansion, globbing or command-string splitting. On Windows, batch scripts require an explicitly chosen interpreter rather than automatic batch execution. Arguments containing NUL are refused before spawning. On Windows, the checked `PATH`/`PATHEXT` candidate is pinned for execution, so the child's executable spelling may be an absolute path rather than the caller's original first argument; a `cwd` or `env` option is applied to that search, so relative `PATH` entries are resolved against the child's directory.

```ae
proc_run(["git", "--version"])
proc_run(["git", "status", "--short"], {encoding: "utf-8"})
proc_run(["legacy-tool.exe", "argument with spaces"], {encoding: "gbk"})
proc_run(["binary-tool.exe"], {encoding: "bytes"})
proc_run(["build.sh"], {cwd: "tools", env: {CC: "clang"}, stdout_log: "build.log"})
proc_run(["jq", "."], {stdin: "{\"a\":1}", capture: "tail"})
```

These are executable examples, not portable fixtures: the named programs must be installed and the execution policy must permit them. The builtin is classified as `exec` and applies the same execution policy as other caller-chosen programs. It is not a background-job or persistent-process interface. If the chosen executable is a shell or code interpreter, code passed to it still follows that interpreter's quoting and expansion rules; an argv array does not make executable source inert.

## Options

| Field | Default | Accepted values |
|---|---|---|
| `encoding` | `utf-8` | `utf-8`, `gbk`, `utf-16le`, `utf-16be`, or `bytes` |
| `timeout_ms` | `120000` | Integer from `1` through `600000` |
| `max_output_bytes` | `1048576` | Integer from `1` through `16777216`, for each stream |
| `capture` | `prefix` | `prefix` or `tail` |
| `on_output_limit` | `terminate` | `terminate` or `continue` |
| `cwd` | inherited | A non-empty String naming an existing directory |
| `env` | inherited | A Record of String names to String values or `null` |
| `stdin` | none | A String, sent as its exact UTF-8 bytes |
| `stdin_base64` | none | Standard base64, sent as the decoded bytes |
| `stdout_log` | none | A String path for a new file |
| `stderr_log` | none | A String path for a different new file |
| `max_log_bytes` | `67108864` | Integer from `1` through `1073741824`, for each log |

Unknown option names, unsupported encodings or capture labels, invalid argument types and out-of-range integers are errors before the program starts. No locale, Windows ANSI/OEM codepage, or content-based encoding guess is used. The selected text encoding applies to stdout and stderr independently; one stream may decode successfully while the other fails.

`cwd` is resolved against the interpreter's own working directory, canonicalized, and checked to be an existing directory before anything runs. The interpreter never changes its own directory, and the child is started in the validated directory.

`env` changes the child's environment only: a String sets a name, `null` removes one inherited name, and every other inherited name — including any credential the deployment did not scrub — stays as it was. Names must be non-empty and contain no `=` or NUL, and values must contain no NUL. Names in the `DSH_*`, `AETHER_*` and `AE_*` namespaces are refused, as are names that collide case-insensitively on Windows. That namespace check is a convenience guard, not a security boundary: an `argv` can still start a program that sets its own environment. Environment values are never echoed in a result, an error, or an audit entry.

`stdin` sends exactly the string's UTF-8 bytes with nothing appended; `stdin_base64` sends arbitrary bytes and is strictly decoded, so invalid base64 is refused before the program starts. The two are mutually exclusive, and 16 MiB is the protocol maximum for either. A writer thread feeds the child while both stream readers drain, so a child that writes before it reads cannot deadlock the capture. A requested stdin the operating-system pipe did not fully accept — a child that closed its input early, or a timeout that stopped it — leaves `stdin.complete` false with `stdin.error` set, which clears the overall `success` while `process.success` still reports the child's own exit status.

## Result

A captured process returns a record even when it exits nonzero or produces undecodable output. Process-start and capture-infrastructure failures are coded builtin errors. The record contains:

- `success`: the complete operation succeeded: the process exited zero, neither a timeout nor an output limit stopped it, cleanup completed, both captures are complete, every requested log is complete, any requested stdin was fully delivered, and any requested text decoding succeeded. A bounded preview of a longer stream therefore reports `success: false`; use `process.success` to read the child's own outcome.
- `process`: `success` for the child's exit status alone, `exit_code` (or null when unavailable), `timed_out`, `output_limit_exceeded`, `group_termination_requested`, and `cleanup_complete`. The termination flag acknowledges an accepted request or an already-gone group; cleanup means the leader was reaped and the capture-reader and stdin-writer threads were joined, not that every descendant is known to have exited.
- `stdin`: `requested`, `bytes` (the bytes offered), `written_bytes` (the bytes the operating-system pipe accepted), `complete`, and `error`. `written_bytes` is never evidence that the child consumed the bytes.
- `stdout` and `stderr`: `encoding`, `capture`, the retained `bytes`, `total_bytes` read from that pipe, `offset_bytes` where the retained bytes start, lossless `base64`, `text`, `decoded`, `complete`, `error`, and the `log` record or `null`.

Strict text decoding never inserts replacement characters. On invalid text, that stream's `text` is null, `decoded` is false, and `error` identifies the decoding failure; its base64 bytes and the process's exit facts remain available. In `bytes` mode, `text` and `decoded` are null because no decoding was requested. An actual U+FFFD character in valid encoded text is data, not evidence of a decoding failure.

BOMs are preserved, not sniffed or stripped. Newlines, trailing whitespace, NUL bytes in output and Unicode normalization are preserved. An explicit but wrong encoding can still produce valid, incorrect text; strict decoding is not encoding detection.

## Capture modes and logs

`capture` chooses which part of a longer stream is retained. `prefix` keeps the first `max_output_bytes`; `tail` keeps the last, and `offset_bytes` says where in `total_bytes` those bytes begin. `bytes` always counts what was retained, `total_bytes` is the exact count read from that pipe, and `base64` always holds exactly the retained bytes. A tail may begin in the middle of a multi-byte character or a UTF-16 code unit; strict decoding then reports `decoded: false` with the raw bytes intact, and `base64` stays an exact, byte-accurate slice. No replacement character is inserted and nothing is trimmed to a text boundary.

`stdout_log` and `stderr_log` name files that receive the raw bytes exactly as read, with no encoding, newline or interleaving applied. Both paths are resolved against the child's effective working directory, their parent directories must already exist, and each file is created exclusively — an existing file is refused, never overwritten, and the two paths must differ. Writing a log is a guarded filesystem effect of the same call, so a denied path creates no file and starts no process. `log.bytes` counts the bytes actually written and `log.complete` is true only when the whole stream read from that pipe reached the file; reaching `max_log_bytes`, or a write failure, leaves a short log with `log.error` set.

## Limits and process ownership

Stdout and stderr are drained concurrently. A capture retains at most its configured byte limit; `on_output_limit` decides what happens when the stream is longer. Under the default `terminate`, the managed process group is asked to stop; under `continue` the child runs on, the capture keeps the retained part and reports `complete: false`, and `process.output_limit_exceeded` stays true. Either way the reader keeps draining so the child cannot block on a full pipe, and a log that cannot be completed also asks the group to stop. The `bytes` field counts bytes retained, not a guessed total output size. A timeout also requests group termination.

For `cwd` and both log paths, the value that was validated is the value that is used: paths are canonicalized and checked to be representable text before the effect gates, and those resolved paths are used for the operation. This does not make path validation and creation atomic against a concurrent filesystem change. The acknowledgement, leader exit and capture cleanup are reported independently; none proves that every descendant has exited. Cleanup waits at most five additional seconds, then reports incomplete resources rather than waiting indefinitely. A descendant that escaped the group may hold a capture open beyond that bound; its reader can remain detached until that pipe closes, and a writer blocked on stdin is bounded by the same deadline. Foreground execution does not preserve background descendants intentionally, and process-group ownership is not a security sandbox. Abrupt termination of the interpreter itself is outside this builtin's in-process cleanup guarantee; DSH's outer managed-process lifetime is separate.

When `cleanup_complete` is false, input, stream and log counters are progress snapshots rather than final totals, and detached I/O can finish later. Do not treat an incomplete log as a stable artifact or automatically replay the command.

The base64 representation increases result size. The DSH adapter can retain a large complete result for paging within its acquisition and storage limits; beyond those limits it explicitly refuses a result even when the native capture stayed within its own bound. Request smaller output, a smaller `max_output_bytes`, or a `tail` capture; a tool result must not silently discard part of a stream.

## Choosing an interface

- Use `proc_run` for explicit argv, an explicit child environment, and strict text or byte capture.
- Use `stdout_log`/`stderr_log` when the whole stream must survive the result budget; the log holds raw bytes even when the returned capture is a bounded preview.
- Keep existing `sh` consumers when their legacy result is required. `sh` still returns its existing fields and may replace invalid UTF-8 output; this new interface does not silently change that behavior.
- Use typed Agent API arguments to pass data without source escaping. In source, use `r"..."` or hash-delimited raw literals for paths, regular expressions and templates; ordinary strings retain interpolation semantics.

# Explicit process output and encoding

`proc_run(argv, options?)`, also available as `proc.run`, runs a foreground program directly. `argv` is a non-empty array of strings: the first item names the executable and each remaining item is one literal argument. There is no implicit shell, variable expansion, globbing or command-string splitting. On Windows, batch scripts require an explicitly chosen interpreter rather than automatic batch execution. Arguments containing NUL are refused before spawning. On Windows, the checked `PATH`/`PATHEXT` candidate is pinned for execution, so the child's executable spelling may be an absolute path rather than the caller's original first argument.

```ae
proc_run(["git", "--version"])
proc_run(["git", "status", "--short"], {encoding: "utf-8"})
proc_run(["legacy-tool.exe", "argument with spaces"], {encoding: "gbk"})
proc_run(["binary-tool.exe"], {encoding: "bytes"})
```

These are executable examples, not portable fixtures: the named programs must be installed and the execution policy must permit them. The builtin is classified as `exec` and applies the same execution policy as other caller-chosen programs. It does not widen DSH file permissions. Its working directory and environment are inherited; stdin is closed. It is not a background-job or persistent-process interface. If the chosen executable is a shell or code interpreter, code passed to it still follows that interpreter's quoting and expansion rules; an argv array does not make executable source inert.

## Options

| Field | Default | Accepted values |
|---|---|---|
| `encoding` | `utf-8` | `utf-8`, `gbk`, `utf-16le`, `utf-16be`, or `bytes` |
| `timeout_ms` | `120000` | Integer from `1` through `600000` |
| `max_output_bytes` | `1048576` | Integer from `1` through `16777216`, for each stream |

Unknown option names, unsupported encodings and invalid argument types are errors before the program starts. No locale, Windows ANSI/OEM codepage, or content-based encoding guess is used. The selected text encoding applies to stdout and stderr independently; one stream may decode successfully while the other fails.

## Result

A captured process returns a record even when it exits nonzero or produces undecodable output. Process-start and capture-infrastructure failures are coded builtin errors. The record contains:

- `success`: the complete operation succeeded: the process exited zero, neither a timeout nor an output limit stopped it, cleanup completed, both captures are complete, and any requested text decoding succeeded.
- `process`: `success` for the child's exit status alone, `exit_code` (or null when unavailable), `timed_out`, `output_limit_exceeded`, `group_termination_requested`, and `cleanup_complete`. The termination flag acknowledges an accepted request or an already-gone group; cleanup means the leader was reaped and capture-reader threads were joined, not that every descendant is known to have exited.
- `stdout` and `stderr`: `encoding`, captured `bytes`, lossless `base64`, `text`, `decoded`, `complete`, and `error`.

Strict text decoding never inserts replacement characters. On invalid text, that stream's `text` is null, `decoded` is false, and `error` identifies the decoding failure; its base64 bytes and the process's exit facts remain available. In `bytes` mode, `text` and `decoded` are null because no decoding was requested. An actual U+FFFD character in valid encoded text is data, not evidence of a decoding failure.

BOMs are preserved, not sniffed or stripped. Newlines, trailing whitespace, NUL bytes in output and Unicode normalization are preserved. An explicit but wrong encoding can still produce valid, incorrect text; strict decoding is not encoding detection.

## Limits and process ownership

Stdout and stderr are drained concurrently. Each capture retains at most its configured byte limit; exceeding a limit requests termination of the managed process group and returns an explicitly incomplete capture rather than an apparently complete prefix. The `bytes` field counts bytes retained, not a guessed total output size. A timeout also requests group termination. The acknowledgement, leader exit and capture cleanup are reported independently; none proves that every descendant has exited. Cleanup waits at most five additional seconds, then reports incomplete resources rather than waiting indefinitely. A descendant that escaped the group may hold a capture open beyond that bound; its reader can remain detached until that pipe closes. Foreground execution does not preserve background descendants intentionally, and process-group ownership is not a security sandbox. Abrupt termination of the interpreter itself is outside this builtin's in-process cleanup guarantee; DSH's outer managed-process lifetime is separate.

The base64 representation increases result size. DSH may reject a result exceeding its complete-result budget even when the native byte capture stayed within its limit. Request smaller output or a smaller `max_output_bytes`; a tool result must not silently discard part of a stream.

## Choosing an interface

- Use `proc_run` for explicit argv and strict text or byte capture.
- Keep existing `sh` consumers when their legacy result is required. `sh` still returns its existing fields and may replace invalid UTF-8 output; this new interface does not silently change that behavior.
- Use typed Agent API arguments to pass data without source escaping. In source, use `r"..."` or hash-delimited raw literals for paths, regular expressions and templates; ordinary strings retain interpolation semantics.

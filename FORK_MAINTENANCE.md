# Ryan's AetherShell build

This fork is maintained at https://github.com/RyanZhou416/AetherShell and starts from upstream `master` commit `924832761bc27eb58a74a455a9255b451e52079e`. The local primary checkout is `C:\Project\AetherShell`. The upstream remains https://github.com/nervosys/AetherShell; its AGPL-3.0-or-later license and notices are preserved. This is not an official release or a copy inside the DSH repository.

## Runtime identity

The source build identifies itself as `12.0.2+ryan.5`. That build-metadata suffix distinguishes it from the official `12.0.2` precompiled package without claiming a new upstream release. The lockfile pins the process-group dependency and its platform support; `encoding_rs` is already present in the dependency graph. Building this version does not replace an installed interpreter.

## Behavior to preserve

- The upstream development branch's JSON-null parsing fix is retained. JSON serialization also emits native nulls as JSON nulls, including nested values, while real strings remain strings.
- `sleep` uses finite, non-negative seconds representable as a duration, including fractions. NaN, infinity and overflowing durations return argument errors rather than panicking the interpreter. Its native discovery entry and examples state the unit, and the book does not advertise milliseconds.
- Sorting keeps the supported natural-order and field/key forms. Unsupported comparator arguments fail visibly rather than being mistaken for a supported sort order.
- Discovery entries for the touched operations describe actual positional and pipeline arguments, with examples covered by regression tests. No blanket rewrite of the builtin registry is implied.
- Agent API calls and pipelines pass decoded JSON values directly to builtin dispatch. Argument strings and record keys are data, never reconstructed AetherShell source. Predicates and explicit `eval` programs remain executable source. Omitted pipeline input and an explicit JSON null are distinct.
- The `capabilities` request advertises the typed-data protocol with explicit version and feature fields. Consumers must check it before removing protections needed by older interpreters; a release label alone is insufficient.
- `len` and `str.len` retain their existing UTF-8 byte count for strings. Use `byte_len` for an explicit byte count and `char_len` for Unicode scalar values, not user-perceived grapheme clusters; both are also available under `str`.
- Ordinary double-quoted strings accept only `\\`, `\"`, `\n`, `\r` and `\t` escapes. Unknown escapes and unclosed strings are parse errors before evaluation. Ordinary single-quote interpolation is unchanged. Raw `r"..."` and hash-delimited raw literals preserve their contents without escapes, interpolation, newline conversion or Unicode normalization. Generated Bash, Zsh and PowerShell literals use one source encoder, including a non-interpolating fallback when all raw delimiters collide with the text.
- `proc_run` accepts an argv array and explicit encoding options, retains base64 bytes, and reports process, decoding and output-completeness facts independently. It is execution-gated, inherits DSH file restrictions, and does not alter legacy `sh` output. Use explicit encodings rather than locale or codepage guesses. [Process execution](docs/api/PROCESS_RUN.md) owns its result fields and bounds.
- `file_read_text` provides bounded raw-byte snapshots with strict named encodings or BOM-selected decoding; its explicit BOM policy must not silently guess, replace malformed text or normalize newlines. `file_write_verified` encodes a String, publishes through an exclusive sibling temporary file, and rereads the target before reporting verification. Failed post-write verification reports the write honestly and never rolls back a concurrent writer. Digest preconditions are checks, not cross-process compare-and-swap. Legacy write/append/patch semantics stay unchanged and discovery states their weaker guarantees. [File text I/O](docs/api/FILE_TEXT_IO.md) owns the options and result fields.
- Ordinary String interpolation prepares every hole in the String before evaluating any hole. Each hole must be one expression with a closing brace; parse failures and runtime failures propagate instead of becoming successful diagnostic text. Raw literals and valid ordinary interpolation, including `${'$'}`, retain their behavior. Earlier program statements and earlier evaluated holes are not rolled back on a runtime error.
- `group_by` keeps full typed keys distinct even when their display labels match. All-Int `min`/`max` aggregates preserve native i64 precision. JSON conversion maps non-finite Float values to null rather than zero. Unicode diagnostic previews use scalar boundaries; file patch/replace rejects empty search text as a reported patch failure, preserves whitespace-only patterns, and performs no writeback when no patch applied. DSH's exact-JSON-number limit remains separate; use String values for larger identifiers.
- Native CLI, Agent API, and in-memory `sess_*` session behavior remain available for the DSH adapter. Interpreter sessions are live processes, not checkpointed or replayed commands.

## Build and validation

Use a Windows x64 Visual Studio Developer PowerShell or an equivalent MSVC build environment with Rust installed. Do not substitute the MSYS `link.exe` for the MSVC linker. Invoke the native Cargo executable when a local command wrapper does not propagate its exit code; this machine's native executable is `C:\Users\Ryan\.cargo\bin\cargo.exe`. Build and run focused checks from this checkout:

```powershell
cargo build --release --bins --locked -j 6
cargo test --release --locked --test fork_interpolation_errors --test fork_data_integrity --test fork_unicode_diagnostics --test fork_string_literals --test fork_transpiler_string_fidelity --test fork_typed_agent_api --test sessions -j 6
cargo test --release --locked --test fork_file_read_text --test fork_file_write_verified --test fork_file_discovery --test guard_enforcement --test module_aliases --test effect_snapshot --test effect_alias_agreement --test book_builtins_are_real --test documented_builtin_count -j 6
cargo test --release --locked --lib file_read_text::tests -j 6
cargo test --release --locked --lib file_write_verified::tests -j 6
cargo test --release --locked --test declared_signatures a_self_contained_example_produces_the_result_it_claims -j 6
pwsh -NoProfile -File scripts/verify-dsh-compat.ps1 -Executable ./target/release/ae.exe
```

The DSH adapter has its own interpreter, ownership, background-job, cancellation, and Windows file-policy tests. Run them against this executable before changing the installed runtime. Do not loosen the DSH file policy or enable interpreter-side approvals to make a compatibility check pass.

## Installation and rollback

Keep the official interpreter until the fork's release binaries and compatibility checks pass. Installation into `C:\Program Files\AetherShell` requires an elevated process: back up the previous executables, copy the tested files, and compare their SHA-256 hashes after the copy. Keep the DSH profile's explicit interpreter path unchanged if the same installation directory is used. Do not replace a running interpreter process; close owned test sessions first and let the user restart any unrelated live processes. [Offline activation](docs/api/OFFLINE_ACTIVATION.md) owns the stop/install/start procedure. Its [script](scripts/activate-dsh-aether.ps1) must refuse a live Host or interpreter before modifying files, preserve unrelated profile settings, and never start or stop the Host itself; `-CheckOnly` is read-only.

Generated Cargo output stays in ignored `target/`; binaries, local environment configuration, audit data, and credentials are not committed. Updates are pushed only to the personal fork. Fetch upstream changes deliberately, preserve the regression tests, and validate again before deploying a newer binary.

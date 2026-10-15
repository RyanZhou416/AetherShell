# Ryan's AetherShell build

This fork is maintained at https://github.com/RyanZhou416/AetherShell and starts from upstream `master` commit `924832761bc27eb58a74a455a9255b451e52079e`. The local primary checkout is `C:\Project\AetherShell`. The upstream remains https://github.com/nervosys/AetherShell; its AGPL-3.0-or-later license and notices are preserved. This is not an official release or a copy inside the DSH repository.

## Runtime identity

The binary identifies itself as `12.0.2+ryan.2`. That build-metadata suffix distinguishes this source build from the official `12.0.2` precompiled package without claiming a new upstream release. Cargo's dependency lock remains pinned; the root package version is the only intended lockfile change.

## Behavior to preserve

- The upstream development branch's JSON-null parsing fix is retained. JSON serialization also emits native nulls as JSON nulls, including nested values, while real strings remain strings.
- `sleep` uses finite, non-negative seconds representable as a duration, including fractions. NaN, infinity and overflowing durations return argument errors rather than panicking the interpreter. Its native discovery entry and examples state the unit, and the book does not advertise milliseconds.
- Sorting keeps the supported natural-order and field/key forms. Unsupported comparator arguments fail visibly rather than being mistaken for a supported sort order.
- Discovery entries for the touched operations describe actual positional and pipeline arguments, with examples covered by regression tests. No blanket rewrite of the builtin registry is implied.
- Agent API calls and pipelines pass decoded JSON values directly to builtin dispatch. Argument strings and record keys are data, never reconstructed AetherShell source. Predicates and explicit `eval` programs remain executable source. Omitted pipeline input and an explicit JSON null are distinct.
- The `capabilities` request advertises the typed-data protocol with explicit version and feature fields. Consumers must check it before removing protections needed by older interpreters; a release label alone is insufficient.
- `len` and `str.len` retain their existing UTF-8 byte count for strings. Use `byte_len` for an explicit byte count and `char_len` for Unicode scalar values, not user-perceived grapheme clusters; both are also available under `str`.
- Native CLI, Agent API, and in-memory `sess_*` session behavior remain available for the DSH adapter. Interpreter sessions are live processes, not checkpointed or replayed commands.

## Build and validation

Use a Windows x64 Visual Studio Developer PowerShell or an equivalent MSVC build environment with Rust installed. Do not substitute the MSYS `link.exe` for the MSVC linker. Invoke the native Cargo executable when a local command wrapper does not propagate its exit code; this machine's native executable is `C:\Users\Ryan\.cargo\bin\cargo.exe`. Build and run focused checks from this checkout:

```powershell
cargo build --release --bins --locked -j 6
cargo test --release --locked --test fork_typed_agent_api --test fork_unicode_lengths --test fork_process_discovery --test agent_api --test fork_json_roundtrip --test fork_sort_args --test fork_discovery_contracts --test sessions --test sort_by_key --test reliability --test book_builtins_are_real --test documented_builtin_count -j 6
cargo test --release --locked --test declared_signatures a_self_contained_example_produces_the_result_it_claims -j 6
pwsh -NoProfile -File scripts/verify-dsh-compat.ps1 -Executable ./target/release/ae.exe
```

The DSH adapter has its own interpreter, ownership, background-job, cancellation, and Windows file-policy tests. Run them against this executable before changing the installed runtime. Do not loosen the DSH file policy or enable interpreter-side approvals to make a compatibility check pass.

## Installation and rollback

Keep the official interpreter until the fork's release binaries and compatibility checks pass. Installation into `C:\Program Files\AetherShell` requires an elevated process: back up the previous executables, copy the tested files, and compare their SHA-256 hashes after the copy. Keep the DSH profile's explicit interpreter path unchanged if the same installation directory is used. Do not replace a running interpreter process; close owned test sessions first and let the user restart any unrelated live processes.

Generated Cargo output stays in ignored `target/`; binaries, local environment configuration, audit data, and credentials are not committed. Updates are pushed only to the personal fork. Fetch upstream changes deliberately, preserve the regression tests, and validate again before deploying a newer binary.

# Explicit file decoding and verified text writes

Use `file_read_text(path, options?)` / `file.read_text` for a bounded byte snapshot and explicit decoding. Use `file_write_verified(path, content, options?)` / `file.write_verified` when a String write must be followed by an independent byte-level readback. These operations use the existing native path policy and inherit any DSH file restrictions; they do not change permissions or retry denied access.

Native path checks and DSH confinement are separate. A DSH backend reporting partial enforcement is not a complete write-isolation guarantee; Windows restricted-token rules can leave writes permitted by ambient Everyone ACLs. These file operations neither widen nor repair that backend. Do not infer permission enforcement from a requested mode alone.

The Agent API envelope states whether the builtin returned a value. It is not the operation's own success flag. A returned read record may contain a decoding failure, and a returned write record may state that a write happened but verification failed. Check those fields before continuing.

## Read options

| Option | Default | Values |
|---|---|---|
| `encoding` | `utf-8` | `utf-8`, `gbk`, `utf-16le`, `utf-16be`, `bom`, `bytes` |
| `bom` | `preserve` | `preserve`, `strip`, `require`, `forbid` |
| `max_bytes` | `1048576` | Integer `1..16777216`; the existing global file-size policy also applies |

```ae
file_read_text("legacy.txt", {encoding: "gbk"})
file_read_text("marked.txt", {encoding: "bom", bom: "strip"})
file_read_text("payload.bin", {encoding: "bytes"})
```

`encoding: "bom"` selects only a recognized leading UTF-8, UTF-16LE or UTF-16BE mark. Without one, decoding fails explicitly; no locale or content-based guess is made. An explicitly named encoding is never overridden by a different mark. The supported mark prefixes do not imply support for UTF-32 or every file format that begins with similar bytes.

- `preserve` retains a matching mark as U+FEFF in text.
- `strip` removes only the matching leading mark from text, not from the raw snapshot.
- `require` requires a matching mark and preserves it in text.
- `forbid` reports a decoding failure when a recognized mark is present.
- `bytes` performs no decoding and accepts only `preserve`.

The result is `{success, path, bytes, base64, sha256, complete, requested_encoding, encoding, decoded, text, bom:{detected,bytes,policy}, error}`. Raw bytes and their SHA-256 always describe the full snapshot, including any mark. `complete` is true for a returned record: policy and I/O failures are coded errors, while a valid request exceeding the native or caller's size limit returns `E_BUDGET_EXCEEDED` rather than a partial read. Invalid option values remain `E_BAD_ARG`. Invalid text or an incompatible BOM produces `success:false`, `decoded:false`, `text:null`, and an explanatory error while retaining the raw bytes. Byte mode has `text:null` and `decoded:null`. Valid U+FFFD characters remain data; malformed input is never replaced with U+FFFD. CRLF, LF, tabs, NUL and Unicode normalization are not transformed.

Base64 increases output size. DSH's complete-result limit may be smaller than the native file limit; choose a smaller file or `max_bytes` rather than relying on a truncated result.

## Verified write options

`content` must be a String; arrays and arbitrary debug-rendered values are refused. No pipeline subject is accepted by either new operation.

| Option | Default | Values |
|---|---|---|
| `encoding` | `utf-8` | `utf-8`, `gbk`, `utf-16le`, `utf-16be` |
| `bom` | `none` | `none`, `add`; GBK cannot add a BOM |
| `max_bytes` | `1048576` | Integer `1..16777216`, counting encoded bytes including an added mark |
| `create_only` | `false` | Refuse replacement of an existing path |
| `expected_sha256` | absent | Prior file's 64-hex SHA-256; cannot be combined with `create_only` |

```ae
file_write_verified("output.txt", "甲\r\n乙\n")
file_write_verified("utf16.txt", "中文😀", {encoding: "utf-16le", bom: "add"})
file_write_verified("new.txt", "literal ${'$'}{template}", {create_only: true})
```

Encoding is strict: a GBK-unrepresentable character is refused before any write, not replaced. UTF-16 uses the requested byte order. A U+FEFF already inside `content` is ordinary data, so explicitly adding a BOM can produce two leading marks. No newline, indentation or final-newline conversion is performed.

A private, exclusive temporary file is created beside the destination, written and synced, then published without a copy fallback. Existing permissions are retained where the platform permits; a new file keeps the temporary file's private permissions. Symbolic links and non-regular targets are refused. `create_only` uses no-clobber publication. The digest precondition is checked before publication, including a second check immediately before replacement, but is not an atomic compare-and-swap or a lock against uncooperative writers.

The target is reopened after publication and its bounded bytes are compared with the intended bytes and SHA-256. The result is `{success,path,written,verified,encoding,bom,expected_bytes,observed_bytes,expected_sha256,observed_sha256,precondition_sha256,verification:{method,matched,error}}`. The result's `expected_sha256` describes the intended new bytes; `precondition_sha256` echoes the prior-file condition. `verification.method` is `readback-bytes-sha256`.

Pre-write argument, policy, encoding, precondition and publication failures are coded errors. A readback failure or mismatch after publication returns `written:true`, `verified:false`, `success:false`; it does not automatically roll back or retry. If the file grew beyond the readback bound, no complete observed digest is available: `observed_bytes` and `observed_sha256` are null, with the size failure in `verification.error`. A digest of a truncated prefix is never presented as the file's digest. Another writer's observed bytes must not be overwritten to manufacture a successful verification. A successful readback is an observation at that time, not a guarantee against later changes, directory-level crash durability or power loss. Existing hard links refer to their original inode after atomic replacement; this operation replaces the path rather than editing all aliases in place.

## Legacy operations remain distinct

- `read_text` still returns strict UTF-8 text with its original BOM/newline behavior; `cat` can also accept pipeline input. They do not guess non-UTF-8 encodings.
- `file_write` / `file.write` still renders non-String values, joins arrays using LF, and reports `{path,bytes,lines,success}` without independent readback.
- `file_append` inserts no separator or newline and does not verify the full file. Appending UTF-8 bytes to another encoding can corrupt the document.
- `file_patch` and `file_replace` return their own success and per-patch failures. Valid patches may be written even when other patches fail; the API envelope can succeed while `result.success` is false. An empty search string is a per-patch failure; whitespace-only search text remains a valid literal pattern. If no patch applied, the file is not rewritten. They are not all-or-nothing transactions and do not provide independent post-write verification.

When preserving an existing encoding, read it explicitly, inspect `success` and the raw digest, transform the String deliberately, then choose the matching encoding/BOM on the verified write. Do not infer file encoding from the Host locale or an external command's exit code.

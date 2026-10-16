# Core Operations

Core builtins provide essential shell operations: output, environment management, JSON handling, option types, diagnostics, and shell interop.

## Output

### `print`
Print a value to stdout without a trailing newline.

```aethershell
print "hello"       # hello
print 42            # 42
print [1, 2, 3]     # [1, 2, 3]
```

### `echo`
Print a value followed by a newline. Equivalent to `println` in many languages.

```aethershell
echo "Hello, world!"
echo { name: "Ada", age: 36 }
```

### `debug` / `dbg`
Print a value with type and structure information, useful for development.

```aethershell
debug [1, "two", 3.0]
# Array(3): [Int(1), String("two"), Float(3.0)]

let rec = { x: 1, y: [2, 3] }
dbg rec
```

## Help & Inspection

### `help`
Display available commands and usage information.

```aethershell
help              # List all builtins
help "map"        # Help for a specific builtin
```

### `type_of` / `typeof`
Return the type name of a value as a string.

```aethershell
type_of 42           # "Int"
type_of "hello"      # "String"
type_of [1, 2, 3]    # "Array"
type_of { x: 1 }     # "Record"
typeof fn(x) => x    # "Lambda"
```

### `inspect`
Return a detailed string representation of a value including internal structure.

```aethershell
inspect [1, "two", true]
# "[Int(1), String(\"two\"), Bool(true)]"
```

### `len` / `char_len` / `byte_len`
Length depends on what is being measured, so the shell names each measure instead
of silently choosing one for you. `len` counts a string's UTF-8 bytes, an array's
elements, and a record's fields; `byte_len` is the same byte count for a string
stated explicitly; `char_len` counts Unicode scalar values. Grapheme clusters are
not counted.

```aethershell
len [1, 2, 3]        # 3
len "你好"           # 6
char_len "你好"      # 2
```

See [`len`](./collections.md#len--length) for the container forms and
[String Operations](./strings.md#length) for the string measures.

## Option Types

AetherShell has first-class `Some`/`None` for representing optional values.

### `Some`
Wrap a value in an option.

```aethershell
let result = Some(42)
echo result          # Some(42)
```

### `None`
The empty option value.

```aethershell
let missing = None
echo missing         # None
```

Options are useful in pipelines where operations may not find a result:

```aethershell
let found = [1, 2, 3] | first
# found is Some(1) or None if array is empty
```

## Environment Variables

### `env`
Return all environment variables as a Record.

```aethershell
let vars = env
echo vars.PATH
echo vars.HOME
```

### `set_env`
Set an environment variable for the current session.

```aethershell
set_env "MY_VAR" "hello"
echo (env).MY_VAR    # hello
```

## Explicit file text I/O

`file_read_text(path, options?)` (`file.read_text`) returns a bounded raw snapshot, its SHA-256, BOM facts and strict decoding status. Choose `utf-8`, `gbk`, `utf-16le`, `utf-16be`, `bom` or `bytes`; a missing mark in BOM-selected mode is an explicit failure, not an encoding guess. BOM policy defaults to preserving it, and stripping is explicit. Inspect the returned `success`, `decoded`, `text` and `error` before using text.

`file_write_verified(path, content, options?)` (`file.write_verified`) strictly encodes a String, publishes through a private sibling temporary file, then reopens the target and compares bytes and SHA-256. Inspect `written` and `verified` independently: a post-write verification failure does not undo the write or overwrite another writer's bytes. Digest preconditions are checks rather than cross-process transactions.

```aethershell
file_read_text("marked.txt", {encoding: "bom", bom: "strip"})
file_write_verified("output.txt", "甲\r\n乙\n", {encoding: "utf-8"})
```

The examples operate on real files under the current path policy. [File text I/O](../../../api/FILE_TEXT_IO.md) documents limits, BOM behavior, result fields and the distinct guarantees of legacy write, append and patch operations. Neither new interface normalizes line endings or guesses the Host codepage.

## JSON

### `json_parse`
Parse a JSON string into a structured Value (Record, Array, etc.).

```aethershell
let data = json_parse '{"name": "Ada", "langs": ["Rust", "Python"]}'
echo data.name       # Ada
echo data.langs[0]   # Rust
```

### `json_stringify`
Serialize any value to a JSON string.

```aethershell
let rec = { x: 1, y: [2, 3] }
let s = json_stringify rec
echo s               # {"x":1,"y":[2,3]}
```

JSON nulls retain their native null type through parsing and serialization, including inside arrays and records. The strings `"null"` and `"<null>"` remain strings.

```aethershell
json_parse("null") == None             # true
json_stringify({missing: None})        # "{\"missing\":null}"
```

### `save_json` / `write_json`
Write a value as formatted JSON to a file.

```aethershell
let config = { debug: true, port: 8080 }
save_json "config.json" config
```

## Timing & Sleep

### `time`
Measure execution time of an expression. Returns the elapsed time.

```aethershell
time (ls "." | where(fn(f) => f.size > 1000))
# Elapsed: 12ms
```

### `now` / `timestamp`
Return the current Unix timestamp in milliseconds.

```aethershell
let start = now
# ... do work ...
let elapsed = now - start
echo "Took ${elapsed}ms"
```

### `sleep`
Pause execution for a non-negative duration in seconds. Fractional seconds are accepted.

```aethershell
sleep(1)             # Sleep for 1 second
sleep(0.1)           # Sleep for 100 milliseconds
```

## Shell Interop

### `sh` / `shell`
Execute a program and return the legacy `{stdout, stderr, success, exit_code}` record. A string argument is split into words; an argv array avoids that parsing. No implicit shell evaluates pipes or redirects. The interpreter's `sh` switch must permit the call. Legacy output uses lossy UTF-8 decoding; use `proc_run` when decoding errors or binary fidelity matter.

```aethershell
let result = sh(["git", "status", "--short"])
result.stdout
```

### `proc_run`
Run a non-empty argv array directly, with explicit output encoding and separate process/decoding facts. It accepts `utf-8` (strict default), `gbk`, `utf-16le`, `utf-16be`, or `bytes`, with bounded concurrent stdout/stderr capture. Both streams retain base64 bytes even when strict text decoding fails. No BOM stripping, encoding guessing or newline conversion is performed; the execution policy still applies.

```aethershell
proc_run(["git", "--version"], {encoding: "utf-8"})
proc_run(["binary-tool.exe"], {encoding: "bytes", max_output_bytes: 4096})
```

The programs in these examples must be installed; `success` on the returned record is different from the Agent API envelope's success. See [explicit process output](../../../api/PROCESS_RUN.md) for options, result fields, timeout and output-limit behavior.

### `call`
Call a function or builtin by name (as a string).

```aethershell
call "echo" "hello"
let op = "upper"
call op "hello"      # "HELLO"
```

### `exit`
Exit the shell with an optional exit code.

```aethershell
exit           # Exit with code 0
exit 1         # Exit with code 1
```

## Diagnostics

### `assert`
Assert that a condition is true. Throws an error if false.

```aethershell
assert (2 + 2 == 4)            # passes
assert (len [1,2,3] == 3)      # passes
assert false                    # ERROR: assertion failed
```

### `type_assert` / `assert_type`
Assert that a value has a specific type.

```aethershell
type_assert 42 "Int"            # passes
type_assert "hi" "String"       # passes
type_assert 42 "String"         # ERROR: expected String, got Int
```

### `is_error`
Check whether a value is an Error.

```aethershell
let result = try { json_parse "invalid" } catch(e) { e }
echo (is_error result)          # true

echo (is_error 42)              # false
```

### `trace`
Print a trace message with context, useful for debugging pipelines.

```aethershell
[1, 2, 3]
  | map(fn(x) => { trace "processing" x; x * 2 })
  | reduce(fn(a, b) => a + b, 0)
```

## Membership

### `in`
Test whether a value exists in an array or a key exists in a record.

```aethershell
echo (3 in [1, 2, 3])           # true
echo ("x" in { x: 1, y: 2 })   # true
echo (5 in [1, 2, 3])           # false
```

## Configuration

### `config`
Display the current shell configuration.

```aethershell
config                   # Show all config
```

### `config_get` / `config_set`
Read or write individual configuration values.

```aethershell
config_get "theme"
config_set "theme" "dark"
config_set "editor" "vim"
```

### `config_path`
Return the path to the configuration file.

```aethershell
echo (config_path)       # ~/.config/aethershell/config.toml
```

### `config_init`
Create a default configuration file.

### `config_reload`
Reload configuration from disk.

### `themes`
List available shell themes.

```aethershell
themes
# ["dark", "light", "monokai", "solarized", ...]
```

# Types and Values

AetherShell is a typed shell where every expression produces a structured `Value`. Unlike traditional shells that pipe raw text, AetherShell pipelines carry rich, typed data.

## Core Types

| Type | Example | Description |
|------|---------|-------------|
| `Null` | `null` | Absence of value |
| `Bool` | `true`, `false` | Boolean |
| `Int` | `42`, `-7` | 64-bit signed integer |
| `Float` | `3.14`, `-0.5` | 64-bit floating point |
| `String` | `"hello"` | Text with interpolation support |
| `Uri` | `openai:gpt-4o-mini` | URI with scheme (RFC 3986) |
| `Array` | `[1, "two", true]` | Heterogeneous ordered list |
| `Record` | `{name: "Ada", age: 36}` | Ordered key-value map |
| `Table` | output of `ls` | Structured table with schema |
| `Lambda` | `fn(x) => x * 2` | First-class function |
| `Error` | `throw "oops"` | Error value |

### Integers and Floats

```ae
let x = 42       # Int
let y = 3.14     # Float
let z = x + y    # Float (auto-promoted)
```

Integer literals and integer match patterns must fit signed i64; out-of-range text is `E_PARSE`, not zero. The integer suffixes `k`/`K`, `M` and `G` multiply by 1,000, 1,000,000 and 1,000,000,000 with the same range check; they never saturate. A minus after an expression is subtraction regardless of spacing: `5-3`, `5 -3` and `5 - 3` all produce `2`. For an unambiguous negative function argument, use parentheses, such as `abs(-7)`, rather than bare word-call syntax. Newlines remain whitespace, so use a semicolon where a separate negative expression is intended.

Scientific notation uses `e` or `E` with an optional exponent sign: `1e3`, `1E+3`, `1.25e-2` and `-2e3` are each one Float literal. A decimal point may precede the exponent, including the trailing-dot spelling `1.e2`. Scientific notation always produces Float, even when the value is a whole number. An exponent without digits (`1e`, `1e+`, `1e-`) is `E_PARSE`; unsupported attached forms such as `1e3foo`, `0x10`, `1_000` or `1.5k` are also refused rather than split into separate expressions. Source Float literals must be finite, so `1e309` is a parse error. Float precision, subnormals and underflow follow f64 semantics; arithmetic can still produce non-finite Float results, whose JSON rendering is null.

Integer division always produces a Float:

```ae
10 / 3   # => 3.3333...
```

### Strings

Ordinary double-quoted and single-quoted strings support `${expr}` interpolation:

```ae
let name = "world"
let greeting = "Hello, ${name}!"    # => "Hello, world!"
let math = "2 + 2 = ${2 + 2}"      # => "2 + 2 = 4"
```

Double quotes recognize only `\\`, `\"`, `\n`, `\r` and `\t` escapes. An unknown escape, a dangling backslash or a missing closing quote is an `E_PARSE` error; the program is not evaluated. Single quotes preserve backslashes but still interpolate `${...}`; they are not raw strings. Each interpolation hole must contain exactly one expression and a closing brace. All holes in one String are syntax-checked before any hole in that String is evaluated. Syntax and evaluation failures propagate as errors rather than successful text containing an error message. Earlier statements and earlier successful holes can already have effects when an evaluation fails; this is not a transaction.

Use a raw literal for paths, regular expressions, templates or other opaque text. Raw literals produce ordinary String values without backslash escapes or interpolation. Surround the quotes with matching hashes when the content contains quotes; up to 16 hashes are supported.

```ae
let path = r"C:\temp\new\file.txt"
let regex = r"\d+\s+\w+"
let template = r#"Hello ${name}, say "hello"."#
let quoted_delimiter = r##"contains "# without ending here"##
```

Raw literals may span lines and preserve the exact contents between delimiters, including CRLF, LF, leading/trailing whitespace and Unicode code points. No indentation stripping or normalization is performed. Neither ordinary nor raw strings change the language's statement-separation rules: use semicolons where an explicit end of statement matters.

String concatenation works with `+` and auto-converts the other operand:

```ae
"count: " + 42        # => "count: 42"
100 + " items"         # => "100 items"
```

### URIs

URIs identify resources with a scheme prefix, commonly used for AI model references:

```ae
let model = openai:gpt-4o-mini
let local = ollama:llama3
```

### Arrays

Arrays hold any mix of types:

```ae
let nums = [1, 2, 3]
let mixed = [1, "two", true, [4, 5]]
```

### Records

Records are key-value maps with sorted keys:

```ae
let person = {name: "Ada", age: 36, langs: ["Rust", "Python"]}
person.name    # => "Ada"
person.langs   # => ["Rust", "Python"]
```

### Tables

Tables are structured arrays of records with a defined schema. Many builtins return tables:

```ae
ls "."    # => Table with columns: name, path, ext, is_dir, size, modified
```

Tables get special pretty-printed column-aligned output in the terminal.

## Type Conversion

AetherShell performs automatic numeric promotion in arithmetic:

| Expression | Result Type | Rule |
|-----------|-------------|------|
| `Int + Int` | `Int` | Checked integer arithmetic; overflow is an error |
| `Int + Float` | `Float` | Promote to float |
| `Float + Float` | `Float` | Float arithmetic |
| `Int ^ Int` (positive) | `Int` | Integer power |
| `Int ^ Int` (negative) | `Float` | Float power |
| `Int / Int` | `Float` | Always float division |

Equality comparison between `Int` and `Float` works via casting. All-Int `min` and `max` aggregates keep exact native i64 values; mixed numeric arrays use floating-point semantics. This does not expand the DSH adapter's exact JSON-number range: carry larger opaque identifiers as Strings. Non-finite Float values serialize to JSON null, including inside arrays and records, rather than becoming zero.

Integer addition, subtraction, multiplication, negation and non-negative integer powers refuse results outside signed i64 instead of wrapping. The mathematical remainder of `i64::MIN % -1` is returned as zero. `floor`, `ceil` and zero-digit `round` refuse non-finite or out-of-i64 results instead of saturating. An empty mean is an error; the empty sum is zero. Materialized `range` results are limited to 1,000,000 items and are sized before allocation; a nonzero negative step is supported and integer-boundary steps terminate without wrapping.

JSON text parsed by `json_parse`, `from_json` or `open` for JSON/JSONL must have every integer literal within signed i64. Out-of-range literals are refused even in nested arrays and records; use quoted JSON strings for exact larger identifiers. Decimal and exponent forms retain Float semantics, not arbitrary precision. This check is specific to these text ingestion paths and does not replace every internal JSON adapter. The DSH adapter's narrower exact JSON-number limit remains unchanged.

`group_by` uses the full typed field value for group identity, independently of its display label. Distinct arrays or records, and numeric `1` versus String `"1"`, remain different groups even when their `Name` labels are identical.

## Strict evaluation

An unbound source variable raises `E_UNBOUND_VARIABLE`; it is not an implicit null. A variable explicitly bound to `null` remains valid, and optional/default operations keep their stated behavior. Named arguments are refused when the evaluator does not implement them, before evaluating their argument expressions. These rules apply directly, without a strict-mode switch.

Record-oriented projection, field sorting, grouping, property filtering and numeric measurement refuse a missing requested field or a wrong-typed row rather than inventing empty strings or silently dropping values. Use an explicit optional lookup or transform heterogeneous data deliberately when absence is expected.

## Truthiness

All values have a boolean interpretation:

| Falsy | Truthy |
|-------|--------|
| `null` | Non-null values |
| `false` | `true` |
| `0`, `0.0` | Non-zero numbers |
| `""` (empty string) | Non-empty strings |
| `[]` (empty array) | Non-empty arrays |
| `{}` (empty record) | Non-empty records |
| `Error(...)` | Lambdas, Builtins |

## JSON Interop

Values convert bidirectionally with JSON:

```ae
let data = from_json '{"name": "test", "count": 42}'
data.name    # => "test"

let json_str = to_json {name: "test", count: 42}
# => '{"count":42,"name":"test"}'
```

## Type Inspection

```ae
describe 42          # => "Int"
describe "hello"     # => "String"
describe [1, 2, 3]   # => "Array(3 elements)"
```

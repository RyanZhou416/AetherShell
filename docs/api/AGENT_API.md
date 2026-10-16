# Agent API Reference

The Agent API provides a structured JSON-based HTTP interface for AI agents to interact with AetherShell. Instead of generating shell code, AI agents use typed JSON requests to execute builtins, pipelines, and evaluations.

**Default server:** `http://127.0.0.1:3002`

---

## Authentication

Every endpoint except `GET /health` requires a bearer token:

```
Authorization: Bearer <token>
```

Start the server one of three ways:

```sh
ae agent serve                    # generates a token, prints it at startup
AETHER_API_TOKEN=<token> ae agent serve
ae agent serve --token <token>    # visible in the process list; prefer the env var
```

There is deliberately no way to run with authentication disabled.
`POST /api/v1/eval` evaluates arbitrary AetherShell code, which can spawn
processes, so an unauthenticated listener is remote code execution for anyone
who can reach the port. With CORS enabled (the default), that includes any web
page the user happens to visit while the server is running — the browser will
send the cross-origin request; it just cannot read the token.

A missing or wrong token returns **401 Unauthorized** with
`WWW-Authenticate: Bearer`. `/health` stays open so liveness probes work.

Binding a non-loopback address prints a warning: the token is then the only
thing between an arbitrary-code evaluator and the network. Put it behind TLS —
a bearer token over plain HTTP is readable by anything on the path.

---

## Request deadline

Request/response routes are cancelled with **408 Request Timeout** after
`--request-timeout` seconds (default `300`):

```sh
ae agent serve --request-timeout 60
```

`0` disables the deadline. Prefer raising it: without one, a single
authenticated caller can hold a worker indefinitely, and `POST /api/v1/eval`
evaluates arbitrary code, so a wedged request is a one-line POST.

The SSE `/api/v1/stream/*` routes and `GET /api/v1/ws` are **exempt** — they are
long-lived by design, and a per-request deadline would sever them rather than
protect anything.

Know what the deadline does and does not bound. It frees the connection and the
async worker, and the caller gets its 408. It does **not** stop the evaluation:
the work continues on a blocking-pool thread until it finishes on its own. So a
deliberately wedged request still costs one thread from a bounded pool. The
server stays responsive; it is not immune. If you are running this anywhere
adversarial, bound the *code* you accept, not just the time you wait for it.

---

## Common Response Format

All endpoints return `AgentResponse`:

```json
{
  "success": true,
  "result": <any>,
  "error": "string (on failure)",
  "result_type": "String | Int | Array | Record | ...",
  "metadata": { "structured_data": "typed-v1", "action": "call" }
}
```

- **200 OK** — `success: true`
- **400 Bad Request** — `success: false` with `error` field

The convenience `/api/v1/eval` and `/api/v1/stream/eval` routes require a JSON object containing a String `code`. Missing, null or wrong-typed code is HTTP 400 with `E_BAD_ARG`, not a successful empty program. An explicitly supplied empty or whitespace String remains a valid program. `/api/v1/pipeline` and `/api/v1/stream/pipeline` require an Array `steps`; malformed descriptors retain their validation reason instead of becoming an empty pipeline. An omitted input and `"input": null` remain distinct. Invalid bodies are rejected before evaluation or SSE stream creation. Unknown object fields remain tolerated for compatibility. Axum still owns rejection of syntactically invalid JSON; these body-validation rules concern valid JSON with invalid required fields.

---

## Execution Endpoints

### POST `/api/v1/execute`

Universal entry point. Accepts any `AgentRequest` variant via the `action` discriminator.

**Request body** — one of the tagged union variants:

| `action`        | Fields                                 | Purpose                       |
| --------------- | -------------------------------------- | ----------------------------- |
| `call`          | `builtin: string`, `args: any`         | Execute a single builtin      |
| `pipeline`      | `steps: PipelineStep[]`, `input?: any` | Execute a pipeline            |
| `eval`          | `code: string`                         | Evaluate raw AetherShell code |
| `describe`      | `builtin: string`                      | Get builtin metadata          |
| `list_builtins` | `category?: string`                    | List available builtins       |
| `schema`        | `format?: SchemaFormat`                | Get language ontology/schema  |
| `type_info`     | `type_name?: string`                   | Get type system information   |
| `capabilities`  | none                                   | Report protocol version and typed-data features |

**Example — call:**
```json
POST /api/v1/execute
{
  "action": "call",
  "builtin": "ls",
  "args": ["."]
}
```

**Example — pipeline:**
```json
POST /api/v1/execute
{
  "action": "pipeline",
  "steps": [
    { "builtin": "ls", "args": ["."] },
    { "builtin": "where", "predicate": "size > 1000", "select": "name" }
  ]
}
```

---

### Typed data and capability discovery

The same `AgentRequest` documents are accepted on stdin by `ae agent execute`; no HTTP server is needed for a local caller. `call` arguments and pipeline input are decoded directly into native values. Strings containing `${...}`, quotes, newlines and arbitrary record keys are not parsed as AetherShell source. Only `eval.code` and pipeline predicates are executable source.

Prefer positional argument arrays: `args: [{"a-b": "${1+2}"}]` passes one record unchanged. Omitted/null `args`, an empty array and an empty object mean no arguments. A scalar is one argument. The historical top-level object shim accepts exactly one `input`, `value` or `path` field; mixing one of those fields with other fields is refused instead of discarding data. Other top-level objects are one record argument. An array argument must itself occupy one position, for example `args: [[1,2,3]]`.

Consumers replacing older JSON-to-source behavior must first send `{"action":"capabilities"}` and require the supported protocol version and all needed features. This build returns:

```json
{
  "success": true,
  "result": {
    "protocol": "aethershell-agent-api",
    "protocol_version": 1,
    "features": {
      "typed_arguments_v1": true,
      "typed_pipeline_input_v1": true,
      "explicit_null_pipeline_input_v1": true
    }
  },
  "result_type": "Capabilities"
}
```

Typed call/pipeline responses carry `metadata.structured_data: "typed-v1"` and an `action`; failures identify the builtin where available. They do not claim that generated source ran. Explicit `eval` responses retain `code_executed` or `code_attempted`. Native integers are signed 64-bit; unsigned integers above that range are refused. A JavaScript consumer must impose its own narrower exact-integer limit or carry large identifiers as strings.

Selectors split on dots and treat each segment as a literal record key, including keys such as `a-b` or `中文`; a missing field is an error. Use a complete lambda such as `fn(row) => len(row.items) > 2` for expressions beyond the shorthand predicate forms. An explicit `input: null` starts with native null; omitting input starts from the first step's call.

Argument conversion, predicate syntax and selector-path syntax are checked for every step before the first builtin runs. A predicate must form one lambda expression; constructing it runs no statements. Builtin argument acceptance, policy decisions and data-dependent selector errors are runtime checks. A runtime failure leaves earlier steps' effects in place: a pipeline is a sequence of calls, not a transaction.

### POST `/api/v1/call/:builtin`

Shorthand to call a single builtin by name.

| Parameter  | Location | Type   | Description                              |
| ---------- | -------- | ------ | ---------------------------------------- |
| `:builtin` | path     | string | Builtin function name (e.g., `ls`, `cd`) |
| body       | JSON     | any    | Arguments — object, array, or scalar     |

**Example:**
```json
POST /api/v1/call/ls
["."]
```

---

### POST `/api/v1/pipeline`

Execute a multi-step pipeline.

**Request body:**
```json
{
  "steps": [
    {
      "builtin": "string",
      "args": [],
      "select": "field_name",
      "predicate": "size > 100"
    }
  ],
  "input": [1, 2, 3]
}
```

| Field               | Type           | Required | Description                              |
| ------------------- | -------------- | -------- | ---------------------------------------- |
| `steps`             | PipelineStep[] | yes      | Ordered list of pipeline operations      |
| `steps[].builtin`   | string         | yes      | Function name                            |
| `steps[].args`      | any            | no       | Prefer a positional argument array; predicate replaces it |
| `steps[].select`    | string         | no       | Dot-separated literal field keys projected from each array element |
| `steps[].predicate` | string         | no       | Executable lambda or expression: `size > 100`, `.size > 100`, `x.size > 100`, or `x > 100` |
| `input`             | any            | no       | Initial typed value; explicit null is different from omission |

---

### POST `/api/v1/eval`

Evaluate raw AetherShell code.

**Request body:**
```json
{ "code": "let x = 42; x * 2" }
```

---

## Discovery Endpoints

### GET `/api/v1/schema`

Returns the full language ontology in compact `Ontology` format, including types, builtins, operators, and syntax patterns.

### GET `/api/v1/schema/:format`

Returns the language schema formatted for a specific AI provider's function-calling convention.

| `:format` aliases                 | Provider         |
| --------------------------------- | ---------------- |
| `openai`, `gpt`, `chatgpt`        | OpenAI           |
| `claude`, `anthropic`             | Anthropic Claude |
| `gemini`, `google`                | Google Gemini    |
| `llama`, `meta`, `llama3`         | Meta Llama       |
| `mistral`, `codestral`, `pixtral` | Mistral AI       |
| `cohere`, `command`, `command-r`  | Cohere           |
| `grok`, `xai`                     | xAI Grok         |
| `deepseek`, `deepseek-r1`         | DeepSeek         |
| `bedrock`, `aws`, `amazon`        | AWS Bedrock      |
| `azure`, `azure_openai`           | Azure OpenAI     |
| `qwen`, `alibaba`, `dashscope`    | Alibaba Qwen     |
| `ollama`                          | Ollama           |
| `vllm`                            | vLLM             |
| `huggingface`, `hf`, `tgi`        | HuggingFace TGI  |
| `openrouter`                      | OpenRouter       |
| `kimi`, `moonshot`                | Moonshot Kimi    |
| `yi`, `01ai`                      | 01.AI Yi         |
| `glm`, `chatglm`, `zhipu`         | Zhipu GLM        |
| `reka`                            | Reka AI          |
| `ai21`, `jamba`, `jurassic`       | AI21 Labs        |
| `perplexity`, `sonar`             | Perplexity AI    |
| `together`, `together-ai`         | Together AI      |
| `groq`                            | Groq             |
| `fireworks`, `fireworks-ai`       | Fireworks AI     |
| `json`, `jsonschema`              | Raw JSON Schema  |

### GET `/api/v1/builtins`

List all available builtins.

| Query param | Type   | Description                       |
| ----------- | ------ | --------------------------------- |
| `category`  | string | Filter by category (e.g., `"io"`) |

**Response:**
```json
{
  "success": true,
  "result": {
    "count": 42,
    "builtins": [
      {
        "name": "ls",
        "description": "List directory contents",
        "category": "filesystem",
        "signature": "ls(path?: String) -> Array<Record>"
      }
    ]
  }
}
```

### GET `/api/v1/builtins/:name`

Describe a single builtin with full metadata, parameters, examples, and JSON schema.

**Response (200):**
```json
{
  "success": true,
  "result": {
    "name": "ls",
    "description": "List directory contents",
    "category": "filesystem",
    "signature": "ls(path?: String) -> Array<Record>",
    "parameters": [...],
    "return_type": "Array",
    "examples": [...],
    "json_schema": { ... }
  },
  "result_type": "BuiltinDefinition"
}
```

**Response (404):** `{ "success": false, "error": "Builtin 'xyz' not found" }`

### GET `/api/v1/types`

Returns all AetherShell type definitions (Int, Float, String, Array, Record, Lambda, etc.) with JSON equivalents and field definitions.

---

## Orchestration Endpoints

### GET `/api/v1/orchestration/agents`

List all connected agents (registered via WebSocket).

**Response:**
```json
{
  "success": true,
  "agents": [
    { "id": "agent-1", "capabilities": ["code", "search"], "connected_at": 1707300000 }
  ]
}
```

### GET `/api/v1/orchestration/tasks`

List pending tasks in the orchestration queue.

### POST `/api/v1/orchestration/tasks`

Create a new task.

**Request body:**
```json
{
  "name": "analyze-logs",
  "payload": { "file": "/var/log/app.log" },
  "priority": 5
}
```

**Response (201):**
```json
{ "success": true, "task_id": "uuid" }
```

### POST `/api/v1/orchestration/workflows`

Create a new workflow definition.

**Request body:**
```json
{
  "name": "data-pipeline",
  "steps": [
    {
      "name": "fetch",
      "agent_capability": "http",
      "request": { "action": "call", "builtin": "http_get", "args": { "url": "..." } },
      "on_success": "transform",
      "on_failure": null
    }
  ],
  "context": {}
}
```

**Response (201):** `{ "success": true, "workflow_id": "uuid" }`

### GET `/api/v1/orchestration/workflows`

List all workflows with their status.

### GET `/api/v1/orchestration/workflows/:id`

Get a specific workflow by ID.

**Response (404):** `{ "success": false, "error": "Workflow not found" }`

### POST `/api/v1/orchestration/workflows/:id/cancel`

Cancel a running workflow (sets status to `Failed`). Broadcasts a `workflow_update` event.

### GET `/api/v1/orchestration/metrics`

Get orchestration metrics snapshot.

**Response:**
```json
{
  "success": true,
  "metrics": {
    "agents": { "total": 3, "online": 3 },
    "workflows": { "total": 12, "running": 2, "completed": 8, "failed": 2 },
    "tasks": { "total": 50, "pending": 5 },
    "timestamp": 1707300000
  }
}
```

---

## Marketplace Endpoints

### GET `/api/v1/marketplace/search`

Search the agent marketplace registry.

| Query param | Type   | Description                                  |
| ----------- | ------ | -------------------------------------------- |
| `q`         | string | Search query                                 |
| `category`  | string | Filter by category                           |
| `sort`      | string | Sort: `stars`, `recent`, `name`, `downloads` |
| `page`      | int    | Page number                                  |
| `per_page`  | int    | Results per page                             |

**Response:**
```json
{
  "success": true,
  "query": "code review",
  "agents": [
    {
      "id": "code-reviewer",
      "name": "Code Reviewer",
      "description": "...",
      "author": "acme",
      "version": "1.0.0",
      "downloads": 1500,
      "stars": 42,
      "tags": ["code", "review"],
      "verified": true
    }
  ],
  "total": 15,
  "page": 1,
  "per_page": 20
}
```

### GET `/api/v1/marketplace/agents`

List installed agents.

### POST `/api/v1/marketplace/install`

Install an agent from the marketplace.

**Request body:**
```json
{ "name": "code-reviewer", "version": "1.0.0" }
```

**Response (200):**
```json
{
  "success": true,
  "message": "Installed Code Reviewer v1.0.0",
  "agent": { "name": "code-reviewer", "version": "1.0.0", "path": "..." }
}
```

### POST `/api/v1/marketplace/uninstall`

Uninstall an installed agent.

**Request body:**
```json
{ "name": "code-reviewer" }
```

### POST `/api/v1/marketplace/publish`

Publish an agent definition. Broadcasts an `agent_published` event.

**Request body:**
```json
{
  "name": "my-agent",
  "description": "Does things",
  "systemPrompt": "You are...",
  "tools": ["ls", "cat"],
  "model": "openai:gpt-4o"
}
```

---

## Health

### GET `/health`

```json
{
  "status": "healthy",
  "service": "aethershell-agent-api",
  "version": "0.x.x",
  "features": { "websocket": true, "sse_streaming": true, "orchestration": true },
  "supported_agents": ["openai", "claude", "gemini", ...],
  "schema_formats": { ... }
}
```

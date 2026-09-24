# 16. MCP: operations as tools for agents

The [Model Context Protocol](https://modelcontextprotocol.io) (MCP) is how AI agents (Claude,
ChatGPT, IDE assistants) discover and call tools. With the `mcp` feature, a route becomes a tool
with one attribute option. lesto builds the tool's description from what it already knows about
the route, and a tool call runs the route itself. The code of this chapter is in
`examples/99-tutorial/src/mcp.rs`.

## Setup

```toml
[dependencies]
lesto = { path = "../lesto/crates/lesto", features = ["mcp"] }
```

Mark the operations agents may call, then turn the endpoint on:

```rust
use lesto::mcp::Mcp;

/// Create a note.
///
/// Tags are free text; the same tag can be used on many notes.
#[lesto::post("/notes", status = 201, tag = "notes", mcp = "tool")]
async fn create_note(Json(new): Json<NoteCreate>) -> Json<Note> { .. }

/// Fetch a note by id.
#[lesto::get("/notes/{id}", tag = "notes", responses(404), mcp = "tool")]
async fn get_note(Path(id): Path<u64>) -> Result<Json<Note>, HttpError> { .. }

/// Search notes by title.
#[lesto::get("/notes/search", tag = "notes", mcp(tool, name = "search_notes"))]
async fn search(Query(search): Query<Search>) -> Json<Vec<Note>> { .. }

/// Delete a note: not exposed to agents.
#[lesto::delete("/notes/{id}", status = 204, tag = "notes")]
async fn delete_note(Path(_id): Path<u64>) {}

pub fn mcp_app() -> App {
    App::new()
        .title("Notes")
        .routes(routes![create_note, get_note, search, delete_note])
        .mcp(Mcp::new().instructions("Notes of one user. Search before creating a duplicate."))
}
```

The endpoint is `POST /mcp`. Only routes with `mcp = "tool"` are tools, so `delete_note` stays
out of reach: turning MCP on never exposes a route by accident.

## What an agent sees

`tools/list` describes every tool from the route:

- **name**: the handler's name (`create_note`), or `mcp(tool, name = "..")`. If two exposed
  handlers share a name (two `list` functions in different modules), both use their
  `operationId` instead (`list_notes_get`), which is unique.
- **title and description**: the doc comment, as in the OpenAPI document.
- **inputSchema**: one object built from the path parameters, the query parameters and the
  properties of the JSON body, side by side. For `create_note` that is `title` (required) and
  `tags`. When the body is not an object, or one of its fields has the name of a parameter,
  the body goes under a `body` argument instead.
- **outputSchema**: the schema of the success response.
- **annotations**: hints from the HTTP method. `GET` is read-only; `POST` only adds; `PUT`,
  `PATCH` and `DELETE` are destructive, and `PUT` and `DELETE` are idempotent.

The schemas carry the garde rules, as in the OpenAPI document (`minLength: 1` on `title`), so
the model knows the constraints before it calls.

## What a call does

A tool call becomes an HTTP request to the route: `create_note` with
`{"title": "Call Ann", "tags": ["work"]}` is a `POST /notes` with that JSON body. It goes through
the same extractors, validation, layers and tracing as a request from any other client. The
response becomes the result:

- **success**: the JSON body as `structuredContent`, and as text for clients that do not read
  structured content;
- **error**: the RFC 9457 problem as text, with `isError: true`. A `422` lists every failed
  check, so the model can fix its arguments and try again:

```rust
let result = result(call_tool("create_note", json!({"title": ""}))).await;
assert_eq!(result["isError"], true);
let problem: Value =
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
assert_eq!(problem["status"], 422);
assert_eq!(problem["errors"][0]["pointer"], "/title");
```

A handler can tell that an agent called it: the request carries an `McpCall` extension with the
tool's name.

```rust
async fn get_note(
    Path(id): Path<u64>,
    call: Option<Extension<McpCall>>,
) -> Result<Json<Note>, HttpError> {
    if let Some(Extension(call)) = call {
        lesto::tracing::info!(tool = call.name(), "called by an agent");
    }
    ..
}
```

## Authentication

The headers of the MCP request are forwarded to the route: `Authorization`, API keys, cookies,
`traceparent`. A route that takes `Bearer`, `Jwt` or a store with an `Authenticated` principal
authenticates the agent exactly as it authenticates any other client.

When the route answers `401` (or `403` asking for more scope), the MCP response is that same
`401`, with its `WWW-Authenticate` header. That is the signal MCP clients wait for to start
their OAuth flow. With [`App::protect`](09-security.md), the endpoint is protected like every
other route, so a client needs a token before it can even list the tools.

## Protocol versions

MCP 2026-07-28 is stateless: no handshake, every request carries its protocol version. lesto
speaks it, and also 2025-11-25 and 2025-06-18 for clients that still open with `initialize`.
lesto keeps no session in either case, so the endpoint works behind a load balancer and on AWS
Lambda (chapter 14) with no change. `Mcp::new().legacy(false)` answers only 2026-07-28 clients.

## Connecting a client

Run the app (`lesto dev`, or `LESTO_PORT=8000 cargo run`), then point a client at the endpoint:

```sh
claude mcp add --transport http notes http://127.0.0.1:8000/mcp   # Claude Code
npx @modelcontextprotocol/inspector                               # the MCP Inspector: pick
                                                                  # "Streamable HTTP" and the URL
```

Browsers are held to the same-origin rule: a request with an `Origin` header from another host
is refused with `403`, which protects a local server against DNS rebinding.
`Mcp::allowed_origins(["https://app.example.com"])` lets a web client in.

## Options

```rust
Mcp::new()
    .path("/agents")                    // default /mcp
    .instructions("How to use these tools")
    .allowed_origins(["https://app.example.com"])
    .list_ttl(Duration::from_secs(60))  // how long clients may cache the tool list (default 5 min)
    .legacy(false)                      // 2026-07-28 clients only
```

The endpoint is not part of the OpenAPI document. In a nested app (chapter 10), the tools of
every nested app are included, and only the `mcp(..)` of the app you serve counts.

## Testing

A tool call is a JSON-RPC request, so `oneshot` tests it like any other route (chapter 12). In
2026-07-28 the protocol version goes in `_meta`, and the method and the tool name are repeated
in headers:

```rust
fn call_tool(name: &str, arguments: Value) -> Request<Body> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
            "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"},
        },
    });
    Request::post("/mcp")
        .header("content-type", "application/json")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/call")
        .header("mcp-name", name)
        .body(Body::from(body.to_string()))
        .unwrap()
}
```

## Not there yet

Only tools exist so far. Resources (`mcp = "resource"`) and prompts (`mcp = "prompt"`) are
designed but not implemented, and the macro says so at compile time. The endpoint answers with
one JSON response per request: no streamed progress and no server-initiated requests. There is
no stdio transport, because the clients above connect over HTTP.

## Recap

- `mcp = "tool"` on a route, `App::mcp(Mcp::new())` on the app, feature `mcp`.
- The tool is described from the route: doc comment, parameters and body as arguments, the
  response as the output, hints from the method.
- A call runs the route: validation errors reach the model as `isError` results with the
  problem, a `401` reaches the client as a `401`.
- MCP 2026-07-28 plus the two previous revisions, with no session.

Appendices: [From FastAPI to lesto](A-from-fastapi-to-lesto.md), [Common problems](B-common-problems.md),
[Why axum and not actix-web](C-why-axum.md).

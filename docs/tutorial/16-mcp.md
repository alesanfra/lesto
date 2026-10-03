# 16. MCP: tools, resources and prompts for agents

The [Model Context Protocol](https://modelcontextprotocol.io) (MCP) is how AI agents (Claude,
ChatGPT, IDE assistants) discover and call tools, read context and pick prompts. With the `mcp`
feature, a route becomes one of these with one attribute option:

- a **tool** is something the model decides to call: create a note, search;
- a **resource** is data the user or the client attaches as context: a note's text;
- a **prompt** is a conversation template the user picks, often from a slash menu: "plan this
  note".

lesto builds the description from what it already knows about the route, and every MCP request
runs the route itself. The code of this chapter is in `examples/99-tutorial/src/mcp.rs`;
`examples/02-notes` serves its whole API too, bearer tokens and permissions included, except
`DELETE`.

## Setup

```toml
[dependencies]
lesto = { version = "0.2", features = ["mcp"] }
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

The endpoint is `POST /mcp`. Only routes with an `mcp` option are served, so `delete_note` stays
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
the model knows the constraints before it calls. Two details differ from the OpenAPI document,
for the sake of clients that are stricter than JSON Schema: an `Option<T>` field is written as
`anyOf` a `T` or `null` rather than `"type": [.., "null"]`, and the output schema's root is
written out in full rather than as a `$ref`.

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

## Resources

A `GET` route marked `mcp = "resource"` is data a client can read by URI. The URI is the route's
path under `lesto://{title}`, the app's title as a slug: `GET /notes/{id}/text` of the app
"Notes" is `lesto://notes/notes/{id}/text`. A path with parameters is listed as a resource
*template* (`resources/templates/list`), one without as a resource (`resources/list`).

```rust
/// The text of a note, as Markdown.
#[lesto::get("/notes/{id}/text", tag = "notes", mcp(resource, name = "note_text"))]
async fn note_text(Path(id): Path<u64>) -> Result<(HeaderMap, String), HttpError> {
    if id != 1 {
        return Err(HttpError::not_found(format!("note {id} not found")));
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/markdown".parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "private, max-age=60".parse().unwrap());
    Ok((headers, "# Groceries\n\n- milk".into()))
}
```

Reading `lesto://notes/notes/1/text` is a `GET /notes/1/text` with the MCP request's headers. The
body comes back as the resource's contents, with the response's `Content-Type` as `mimeType`:
text for JSON and `text/*`, base64 for anything else. A `404` is "resource not found".

Clients may cache what they read in MCP 2026-07-28, and the route says for how long the HTTP
way: `Cache-Control: max-age=60` is `ttlMs: 60000`, `public` or `private` is the `cacheScope`.
Without the header nothing is cached, because the data may change and may depend on the caller.

`Mcp::new().resource_base("notes://")` changes the prefix. A resource cannot have a required
query parameter, since its URI has no way to pass one: the app panics at startup and says so.

## Prompts

A `GET` route marked `mcp = "prompt"` returns a `lesto::mcp::Prompt`: the messages the
conversation starts with. Its path and query parameters are the prompt's arguments, with their
doc comments as descriptions.

```rust
#[lesto::model]
pub struct Plan {
    /// When the plan is for, e.g. `this week`.
    #[garde(length(min = 1))]
    when: Option<String>,
}

/// Plan the tasks of a note.
#[lesto::get("/prompts/plan/{id}", tag = "prompts", mcp = "prompt")]
async fn plan(Path(id): Path<u64>, Query(plan): Query<Plan>) -> Result<Prompt, HttpError> {
    if id != 1 {
        return Err(HttpError::not_found(format!("note {id} not found")));
    }
    let when = plan.when.unwrap_or_else(|| "today".into());
    Ok(Prompt::new()
        .user(format!("Turn this note into a plan for {when}:\n\n# Groceries\n\n- milk"))
        .assistant("Here is a plan, one step per line:"))
}
```

`prompts/get` with `{"id": "1", "when": "this week"}` is a `GET /prompts/plan/1?when=this+week`.
A `4xx` from the route (a missing argument, a failed rule, a `404`) is an invalid-params error
carrying the problem, so the client can show what to fix. Over plain HTTP the same route answers
the messages as JSON. A prompt route that returns anything but `Prompt` does not compile.

## Authentication

The headers of the MCP request are forwarded to the route: `Authorization`, API keys, cookies,
`traceparent`. A route that takes `Bearer`, `Jwt` or a store with an `Authenticated` principal
authenticates the agent exactly as it authenticates any other client.

This holds for resources and prompts too. When the route answers `401` (or `403` asking for more scope), the MCP response is that same
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
npx @modelcontextprotocol/inspector@latest --server-url http://127.0.0.1:8000/mcp --transport http
```

The second command opens the [MCP Inspector](https://modelcontextprotocol.io/docs/2026-07-28/tools/inspector)
in the browser (`@latest`: an older copy cached by `npx` may be the deprecated v1). Its command
line mode is handy in scripts, and `--strict` reports tool schemas that some model providers
would reject:

```sh
npx @modelcontextprotocol/inspector@latest --cli http://127.0.0.1:8000/mcp \
    --method tools/list --strict
npx @modelcontextprotocol/inspector@latest --cli http://127.0.0.1:8000/mcp \
    --header "Authorization: Bearer bob-token" \
    --method tools/call --tool-name create_note --tool-arg text=hello
npx @modelcontextprotocol/inspector@latest --cli http://127.0.0.1:8000/mcp \
    --method resources/read --uri lesto://notes/notes/1/text
npx @modelcontextprotocol/inspector@latest --cli http://127.0.0.1:8000/mcp \
    --method prompts/get --prompt-name tidy_note --prompt-args id=1 "audience=the team"
```

The Inspector speaks the 2025 protocol unless told otherwise (`--protocol-era modern`); lesto
answers both.

Browser pages are held back: a request with an `Origin` header is refused with `403` unless the
page and the server are both on this machine (`localhost`, `127.0.0.1`, `[::1]`), or the origin is
listed with `Mcp::allowed_origins(["https://app.example.com"])`. That protects a local server
against DNS rebinding, where a hostile page reaches `127.0.0.1` under its own name. Agents and
command line clients send no `Origin` and are not affected.

## Options

```rust
Mcp::new()
    .path("/agents")                    // default /mcp
    .instructions("How to use these tools")
    .allowed_origins(["https://app.example.com"])
    .resource_base("notes://")          // resource URIs; default lesto://{title}
    .list_ttl(Duration::from_secs(60))  // how long clients may cache the lists (default 5 min)
    .legacy(false)                      // 2026-07-28 clients only
```

The endpoint is not part of the OpenAPI document. In a nested app (chapter 10), the tools,
resources and prompts of every nested app are included, and only the `mcp(..)` of the app you
serve counts.

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

The endpoint answers with one JSON response per request: no streamed progress, no resource
subscriptions and no server-initiated requests. There is no stdio transport, because the clients
above connect over HTTP. RFC 9728's protected resource metadata, which lets a client find the
authorization server on its own, is planned with `oidc`.

## Recap

- `mcp = "tool"`, `"resource"` or `"prompt"` on a route, `App::mcp(Mcp::new())` on the app,
  feature `mcp`.
- A tool is described from the route: doc comment, parameters and body as arguments, the
  response as the output, hints from the method.
- A resource is a `GET` route read by URI (`lesto://{title}{path}`), cached as its
  `Cache-Control` says; a prompt is a `GET` route returning `Prompt`, its parameters as
  arguments.
- Every request runs the route: validation errors reach the model as `isError` results (tools)
  or invalid-params errors (prompts) with the problem, a `401` reaches the client as a `401`.
- MCP 2026-07-28 plus the two previous revisions, with no session.

Appendices: [From FastAPI to lesto](A-from-fastapi-to-lesto.md), [Common problems](B-common-problems.md),
[Why axum and not actix-web](C-why-axum.md).

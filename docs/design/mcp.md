# MCP from the route attribute

Status: phases 1 (tools, both eras) and 2 (resources and prompts) implemented on 2026-09-24;
phase 3 proposed. Roadmap
item 2 in `AGENTS.md` and `README.md`. The open decisions below were taken as recommended.

This document is the written design the roadmap asks for: naming, auth, which operations map to
which MCP primitive, and transport. Open decisions are listed at the end.

## Goal

Let an application expose selected operations to agents over the Model Context Protocol with one
attribute option, reusing everything lesto already derives from types:

```rust
/// Fetch one note.
#[lesto::get("/notes/{id}", mcp = "resource")]
async fn get_note(store: NoteStore, Path(id): Path<i64>) -> Result<Json<Note>, Error> { .. }

/// Create a note.
#[lesto::post("/notes", status = 201, mcp = "tool")]
async fn create_note(store: NoteStore<ReadWrite>, Json(new): Json<NoteCreate>) -> .. { .. }

#[lesto::get("/notes/search", mcp(tool, name = "search_notes"))]
async fn search(store: NoteStore, Query(q): Query<Search>) -> .. { .. }

App::new()
    .routes(routes![get_note, create_note, search])
    .mcp(Mcp::new()) // feature `mcp`: serves POST /mcp
```

## Protocol versions: dual-era

MCP revision **2026-07-28** (final on 2026-07-28) made the protocol stateless: no `initialize`
handshake, no `Mcp-Session-Id`, protocol version and client capabilities on every request in
`_meta`. The specification calls implementations of 2026-07-28 and later *modern*, those of
2025-11-25 and earlier *legacy* (they open with `initialize`), and an implementation that speaks
both *dual-era*.

lesto's server is **dual-era**:

- **Modern (primary)**: 2026-07-28.
- **Legacy (compatibility)**: 2025-11-25 and 2025-06-18, served *statelessly*. The 2025 revisions
  already allow a server that assigns no `Mcp-Session-Id` and opens no SSE stream; `initialize` is
  answered from the static catalog and nothing is remembered between requests.

Why both, confirmed in practice: MCP Inspector v2 (2.8.0) opens ad-hoc URL connections with
the legacy `initialize` unless given `--protocol-era modern`. At the time of writing, the Tier 1 SDKs (TypeScript, Python, Go, C#) and the Rust SDK
(`rmcp` 3.4) support 2026-07-28, but which clients (Claude, ChatGPT, VS Code, Cursor
and others) speak it could not be confirmed. A legacy client talking to a modern-only server
fails with a `400` and has no way to move forward, so a modern-only server would lock those
clients out.

Neither era needs server-side state here, so dual-era costs little: the catalog and the dispatch
are shared, and only the envelope differs (see Era differences). The legacy branch is isolated so
it can be removed once clients have migrated; `Mcp::legacy(false)` turns it off earlier. The
specification's deprecation policy gives at least twelve months of notice before removing a
feature.

### Era selection, per request

1. A request whose `params._meta` carries `io.modelcontextprotocol/protocolVersion` is modern. It
   is served if the version is supported (2026-07-28), otherwise it gets `400` with
   `UnsupportedProtocolVersionError` (`-32022`, `data.supported` listing every supported version,
   both eras).
2. `initialize` is legacy. The version is negotiated as 2025-11-25 prescribes: the client's
   version if supported, otherwise the latest supported legacy version.
3. Any other request is legacy if its `MCP-Protocol-Version` header names a supported legacy
   version.
4. Anything else is refused with `400` and `HeaderMismatch` (`-32020`), whose message names the
   supported versions. A request without `MCP-Protocol-Version` (a 2025-03-26 client) is not
   supported.

The era is chosen per request because there is no session to remember it in. The spec allows a
dual-era server to serve both eras concurrently on the same endpoint.

## Core idea: an MCP call is an in-process HTTP request

`tools/call`, `resources/read` and `prompts/get` are translated into an HTTP request that goes
through the application's own router with `tower::ServiceExt::oneshot`. The translation from
arguments to request is driven by the OpenAPI operation lesto already builds (path and query
parameters, request body schema).

Consequences, all intended:

- Validation is the extractors' own. A garde failure is a `422` problem, which the agent receives
  with `detail` and `errors` and can use to correct its call. The MCP layer does no JSON Schema
  validation and needs no validator crate.
- Authentication and authorization are unchanged: `Bearer`, `ApiKey`, `Jwt`, `lesto::db`
  principals and `App::protect` all run on the inner request.
- The problem layer, the panic catcher, the timeout and the request span all apply. The inner
  request's span is a child of the `/mcp` span, so a trace reads `POST /mcp` → `POST /notes`.
- The route keeps working over plain HTTP; MCP is a second door to the same handler.

Calling the handler function directly was rejected: it needs a request context (extractors,
state, extensions) that only the router can provide.

## Naming and surface

- Feature `mcp` on the `lesto` crate, module `lesto::mcp`.
- `App::mcp(Mcp)` turns the endpoint on. `Mcp` is a builder:
  - `path(..)`, default `/mcp`;
  - `instructions(..)`, returned by `server/discover` (modern) and `initialize` (legacy);
  - `resource_base(..)`, see Resources;
  - `allowed_origins(..)`, see Transport;
  - `resource_url(..)`, see Auth;
  - `list_ttl(..)`, see Caching;
  - `legacy(bool)`, default `true`, see Protocol versions;
  - `serverInfo` defaults to `App::title` / `App::version`.
- Route option, parsed in `RouteArgs`, stored on `RouteMeta::mcp: Option<McpExpose>`:
  - short form: `mcp = "tool" | "resource" | "prompt"`;
  - long form: `mcp(tool, name = "search_notes")`.
  - `RouteMeta::mcp(..)` sets it for `RouteSet::add` users.
- `McpCall { name }` is inserted as a request extension on inner requests, so a handler or a layer
  can tell an MCP call from a direct one.
- The `RouteMeta` field exists without the feature (a small enum, no dependency); it is ignored
  unless `App::mcp` is installed.

Exposure is opt-in per route. Turning MCP on for an app must never make `DELETE /notes/{id}`
reachable by an agent by accident.

### Dependencies

No runtime dependency. JSON-RPC and the subset of MCP types used are hand-written serde structs,
like the OpenAPI model in `openapi.rs`. `base64` (already a dependency) covers binary content and
the `=?base64?..?=` header encoding. `rmcp`, the official Rust SDK, is a **dev-dependency** only,
used as the client in the interoperability tests.

`rmcp` as a runtime dependency was considered. Since 3.4 it supports 2026-07-28 as stable and
2025-11-25 alongside it, serves Streamable HTTP statelessly as a tower service that mounts on
axum, and lets `list_tools` / `call_tool` be implemented by hand (no `#[tool]` macro), so it could
host the catalog. It was not chosen:

- **It would replace the small part.** The catalog, the dispatch through the router and the
  mapping of responses to results are lesto's in either case. `rmcp` would replace the envelope
  (JSON-RPC and MCP types, era selection, header validation, `server/discover`, `initialize`),
  roughly 500 to 600 of an estimated 1000 to 1500 lines.
- **Transport status from the tool call.** `call_tool` returns an MCP result or error, not an HTTP
  response. Turning the inner `401` (or `403 insufficient_scope`) into the status of the
  transport response, which is what starts OAuth in a client, would need a workaround outside the
  SDK, if it is possible at all.
- **The lesto stack.** Request span, `ProblemLayer`, `protect` and the Origin check would sit
  next to the SDK's own validation and errors instead of being the only ones.
- **Dependencies and churn.** `AGENTS.md` prefers no new dependencies; `rmcp` brings a tree of
  its own even with `default-features = false`, and is already at major version 3: each new
  major would be a forced upgrade for lesto.

The cost of the custom implementation is following the specification (four revisions between
2025-03 and 2026-07) and the risk of subtle mistakes in era selection, header decoding and
per-era error codes. The dev-dependency addresses both: the tests drive lesto's server with the
official client in both eras, so a divergence from the specification as the SDK reads it fails
CI, and an `rmcp` upgrade that follows a new revision shows what lesto has to change. This is the
same approach as `mock-oauth2-server` for `oidc`: a real counterpart in the tests, nothing extra
at runtime.

`rmcp` is declared in `[workspace.dependencies]` as
`rmcp = { version = "3.4", default-features = false, features = ["client", "transport-streamable-http-client-reqwest"] }`
and used only in `[dev-dependencies]`, so it adds nothing to a user's build. Checked on
2026-09-24 against `rmcp` 3.4.1, in a scratch crate outside the workspace:

- **Features**: those two are enough. A client written against them compiles and runs:
  `StreamableHttpClientTransport::from_uri`, `list_all_tools`, `call_tool`, `read_resource`,
  `cancel`. Plain `http://` needs no TLS feature, which suits a loopback test.
- **Both eras from the same client**: `ClientServiceExt::serve_with_lifecycle` takes
  `ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] }`
  (modern: `server/discover`, per-request `_meta`, no fallback, so a legacy answer fails the
  test) or `ClientLifecycleMode::Initialize` (legacy handshake, also what plain
  `ServiceExt::serve` does). `Auto` probes and falls back; the tests use the two explicit modes so
  that each era is exercised on purpose.
- **MSRV**: `rmcp` declares `rust-version = "1.88"`, below the workspace's 1.94, and the scratch
  client builds with `cargo +1.94`. This matters: the MSRV job runs
  `cargo check --workspace --all-targets --all-features`, dev-dependencies included.
- **Graph**: `reqwest` resolves to 0.13, the version the workspace already uses. `base64` comes in
  at 0.23 next to the workspace's 0.22; `deny.toml` has `multiple-versions = "allow"`, and it is a
  dev-only duplicate.
- **Licenses**: `rmcp` is Apache-2.0, and `cargo deny check licenses` with the workspace's
  `deny.toml` passes on the client's whole tree.

A side finding that supports the dual-era decision: in 3.4.1, `ProtocolVersion::LATEST` is still
`2025-11-25` and the default `serve` performs the legacy `initialize`. Clients built on the
official Rust SDK therefore speak the legacy era unless their authors opt in to the modern one.

## Mapping operations to MCP primitives

| `mcp =`      | Allowed on                                     | MCP side                                              |
|--------------|------------------------------------------------|-------------------------------------------------------|
| `"tool"`     | any method                                     | `tools/list`, `tools/call`                            |
| `"resource"` | GET only (compile error otherwise)             | `resources/list`, `resources/templates/list`, `resources/read` |
| `"prompt"`   | GET only, return type must be `lesto::mcp::Prompt` | `prompts/list`, `prompts/get`                     |

The GET-only rules and the prompt return type are checked at compile time by the route macro
(the method is known to the macro; the return type goes through a `__private` check whose trait
carries `#[diagnostic::on_unimplemented]`). Names are checked at compile time against the
character set MCP recommends (`[A-Za-z0-9_.-]`, 1 to 128 characters), which also keeps them
header-safe for `Mcp-Name`.

### Tools

- **Name**: `name = ..` if given, otherwise the handler function's name. When two default names
  collide (two `list` handlers in different modules), both fall back to their `operationId`, which
  is unique by construction. Two explicit names that collide panic in `into_router` with a message
  naming both routes, the same way axum treats a route conflict.
- **Order**: `tools/list` returns tools in registration order, which is deterministic, as
  2026-07-28 asks (for client caching and prompt cache hits).
- **`title` / `description`**: the summary and description from the doc comment.
- **`inputSchema`**: one object schema, built from the operation:
  - path parameters (all required);
  - query parameters (required as documented);
  - the properties of the JSON body, merged in flat.

  The body goes under a `body` property instead when it is not an object schema, or when one of
  its property names collides with a path or query parameter. `$ref`s into `components.schemas`
  are carried as `$defs`, so the schema stands alone.
- **Portable schemas** (found with MCP Inspector v2 on 2026-09-24): the root of an output
  schema is inlined, never a bare `$ref`, because the 2025 revisions require `"type": "object"`
  at the top and the TypeScript SDK drops a tool without it (`rmcp` does not, so only a real
  client showed it); and schemars' `"type": ["T", "null"]` for `Option<T>` is rewritten as
  `anyOf: [{"type": "T", ..}, {"type": "null"}]`, which the Inspector's `--strict` flags as a
  portability problem (Gemini's function declarations take a single `type`).
- **`outputSchema`**: the schema of the success JSON response. Modern: any schema (2026-07-28
  accepts any JSON Schema 2020-12). Legacy: only when it is an object schema, which the 2025
  revisions require.
- **Annotations**, derived from the method. MCP's defaults assume the worst (a tool that is not
  read-only is destructive), so every hint is spelled out:
  - GET, HEAD, OPTIONS: `readOnlyHint: true`;
  - POST: `destructiveHint: false` (it adds), `idempotentHint: false`;
  - PUT, DELETE: `destructiveHint: true`, `idempotentHint: true`;
  - PATCH: `destructiveHint: true`, `idempotentHint: false`.
- `x-mcp-header` (mirroring tool parameters into `Mcp-Param-*` headers) is not used: lesto
  routes on the body, not on headers.

### Resources

- The URI is `{resource_base}{path}`. `resource_base` defaults to `lesto://{slug of App::title}`,
  for example `lesto://notes-api/notes/{id}`.
- A route without path parameters is a resource (`resources/list`); a route with parameters is a
  resource template (`resources/templates/list`). axum's `{id}` path syntax is already an RFC 6570
  level 1 template; a wildcard `{*rest}` becomes the reserved expansion `{+rest}`.
- `resources/read` strips the base, matches the path against the fixed resources and then the
  templates segment by segment, and issues the GET with the path (and any query) as given. The
  parameters are not decoded and re-encoded: the URI is already percent-encoded and axum decodes
  it, which is what a direct request would get. The response body becomes `contents` with the
  response's `Content-Type` as `mimeType` (text for JSON and `text/*`, base64 `blob` otherwise).
- A 404, a URI outside the base or one no route matches is "resource not found": `-32602`
  (modern) or `-32002` (legacy), with the URI in `data`. Other failures are `-32603` with the
  problem in `data`, except 401 and 403, which follow the Auth rules below.
- A resource or prompt is listed with the doc comment (`title`, `description`) and the media
  type of its success response (`mimeType`, resources only).
- A resource route with a *required* query parameter panics at startup with a clear message:
  resources have no way to pass one.

### Prompts

- Arguments are the path and query parameters, with their schema descriptions. MCP sends prompt
  arguments as strings; query and path extraction already parse from strings, so any scalar
  parameter works.
- The handler returns `lesto::mcp::Prompt` (or `Result<Prompt, E>`): a list of messages, which
  implements `IntoResponse` (JSON) and `OperationOutput`. Over plain HTTP the same route returns
  the messages as JSON. Its JSON is exactly the `prompts/get` result (`description`,
  `messages`), so the endpoint passes the body through.
- A `4xx` from the route (missing or invalid arguments, a `404`) is `-32602` with the problem in
  `data`, the way MCP reports bad prompt arguments; a `5xx` is `-32603`.

## Dispatch of `tools/call`

1. Split `arguments` using the operation's parameters: path parameters are percent-encoded into
   the template, query parameters are serialized the way `serde_html_form` reads them (arrays as
   repeated keys), the remaining keys (or `body`) become the JSON body.
2. Copy the transport request's headers onto the inner request, except framing and MCP headers
   (`content-length`, `content-type`, `accept`, `host`, hop-by-hop headers, `mcp-*`). This carries
   `Authorization`, API key headers, cookies, `traceparent` and the request id. Modern requests
   may carry `traceparent` / `tracestate` / `baggage` in `_meta` (SEP-414); those, being specific
   to the call, replace the HTTP ones on the inner request. Set `content-type: application/json`
   when there is a body and `accept: application/json`.
3. `oneshot` the inner request on the router.
4. Map the response:
   - 2xx with JSON: `structuredContent` (when an `outputSchema` was declared) plus the serialized
     JSON as a text item in `content`, for clients that ignore structured content;
   - 2xx with `text/*`: a text item;
   - 2xx with anything else: an embedded resource with a base64 `blob`;
   - 204: empty `content`;
   - 4xx and 5xx: `isError: true`, the problem JSON as a text item;
   - exceptions: a `401`, and a `403` carrying `WWW-Authenticate: Bearer error="insufficient_scope"`,
     become the HTTP status of the transport response, with `WWW-Authenticate` copied. That is
     the signal MCP clients use to start the OAuth flow or a scope step-up.
5. An unknown tool name, or `arguments` that is not an object, is JSON-RPC error `-32602`.

### Reaching the router from one of its own routes

The `/mcp` handler needs the finished router, which contains `/mcp` itself, and `into_router`
returns a `Router<S>` whose state arrives later. The plan:

- In `into_router`, build the router with every route, fallback and layer, but without `/mcp`,
  and keep a clone of it (`Router<S>`).
- Add `/mcp` as a route whose `MethodRouter` gets the same layer stack (so it is traced, protected
  and rendered like any other route). Its handler takes `State<S>` and holds the kept router.
- On the first call, `router.with_state(state)` is built once and cached in a
  `OnceLock<Router>`. The state is the one given to `with_state`, the same for the app's lifetime.

CORS and compression are not needed on the inner request and stay outside it.

## Transport

Streamable HTTP, JSON responses only, no server-side state, one `POST` endpoint:

- Every request is answered with `Content-Type: application/json`. Both eras let the server choose
  JSON over an SSE stream per request; lesto never streams (no progress notifications, no
  `notifications/message`).
- Notifications (no `id`, for example the legacy `notifications/initialized`) answer
  `202 Accepted` with no body.
- `GET` and `DELETE` answer `405`: no standalone SSE stream (removed in 2026-07-28, optional
  before) and no session to terminate. An `Mcp-Session-Id` header is ignored and never minted;
  `Last-Event-ID` is ignored.
- JSON-RPC batches are refused with `-32600` (removed from the protocol in 2025-06-18).
- The `Origin` header is validated on every request, which the spec requires against DNS
  rebinding: no `Origin` is allowed (non-browser clients); a loopback origin is allowed when the
  `Host` is loopback too (a local page, such as the MCP Inspector, calling a local server);
  anything else answers `403` unless listed in `Mcp::allowed_origins`. The first version
  allowed an origin equal to the `Host` header, which rebinding defeats: the hostile page's
  origin and the `Host` it sends are the same attacker-chosen name (found on 2026-09-24 while
  comparing with `rmcp`, which validates `Host` against an allowlist instead). A `Host`
  allowlist was not adopted as the default: it breaks every deployment behind a public name
  until configured, while a rebinding attack always carries a non-loopback `Origin`, since
  browsers send `Origin` on every `POST`, same-origin ones included (Fetch standard), and a
  rebinding request is same-origin from the page's point of view.
- `/mcp` is not part of the OpenAPI document, like the docs routes.
- With no server-side state, the endpoint runs unchanged behind a load balancer and on AWS
  Lambda (`lesto::lambda`).
- stdio is out of scope: the clients in question speak HTTP. A `lesto mcp` bridge in `lesto-cli`
  could be added later.

### Modern requests (2026-07-28)

- Methods: `server/discover` (required by the spec), `tools/list`, `tools/call`,
  `resources/list`, `resources/templates/list`, `resources/read`, `prompts/list`, `prompts/get`.
- Header validation, rejected with `400` and `HeaderMismatch` (`-32020`) when it fails:
  - `MCP-Protocol-Version` is present and equals `_meta`'s `io.modelcontextprotocol/protocolVersion`;
  - `Mcp-Method` is present and equals `method`;
  - each of these headers appears once (a repeated one could be read one way by an intermediary
    and another way by the server);
  - `Mcp-Name` is present on `tools/call`, `resources/read` and `prompts/get` and equals
    `params.name` / `params.uri`, after decoding the `=?base64?..?=` form.
- An unknown method answers `404` with `-32601`, which is how a modern client tells a modern
  server from a legacy one. `subscriptions/listen`, the tasks extension and multi round-trip
  requests are not supported: they are not in the capabilities, so conforming clients do not
  call them.
- Every result carries `resultType: "complete"` and `_meta["io.modelcontextprotocol/serverInfo"]`.
- `io.modelcontextprotocol/clientCapabilities` is read but never required, so
  `MissingRequiredClientCapability` is never returned. `io.modelcontextprotocol/logLevel` is
  ignored: logging is deprecated, and without the field the server must not send
  `notifications/message` anyway.
- `server/discover` returns `supportedVersions` (modern and legacy), `capabilities` (only the
  kinds the app exposes), `instructions`, `serverInfo` in `_meta`, and the caching fields.

### Legacy requests (2025-11-25, 2025-06-18)

- Methods: `initialize`, `notifications/initialized`, `ping`, and the same list/call/read/get
  methods as modern.
- `initialize` answers with the negotiated version, the same capabilities (`listChanged: false`),
  `serverInfo` and `instructions`, and no `Mcp-Session-Id`.
- Results have no `resultType`, no caching fields and no `serverInfo` in `_meta`.
- Error codes and status codes as the 2025-11-25 transport specifies.

### Era differences, in one place

| Aspect                        | Modern (2026-07-28)                         | Legacy (2025-11-25, 2025-06-18)        |
|-------------------------------|---------------------------------------------|----------------------------------------|
| Opening                       | none; `server/discover` optional for client | `initialize` + `notifications/initialized` |
| Version carried in            | `_meta` + `MCP-Protocol-Version`            | `MCP-Protocol-Version` after `initialize` |
| `Mcp-Method` / `Mcp-Name`     | required, validated                         | not used                               |
| `ping`                        | removed (`-32601`)                          | answered                               |
| `resultType`                  | `"complete"` on every result                | absent                                 |
| `ttlMs` / `cacheScope`        | on lists, `resources/read`, `server/discover` | absent                               |
| `outputSchema`                | any JSON Schema                             | object schemas only                    |
| Resource not found            | `-32602`                                    | `-32002`                               |
| Unsupported version           | `400`, `-32022`, `data.supported`           | negotiated down in `initialize`        |

In code the era is a value (`Era::Modern` / `Era::Legacy`) passed to the result builders, not two
copies of the dispatch: `protocol/modern.rs` and `protocol/legacy.rs` hold the envelopes and the
era-specific validation, everything else is shared.

### Caching (modern)

2026-07-28 requires `ttlMs` and `cacheScope` on list results, `resources/read` and
`server/discover`.

- Lists and `server/discover`: the catalog is fixed for the life of the process, so `ttlMs` is
  `Mcp::list_ttl` (default 5 minutes, short enough for a redeploy to show up). `cacheScope` is
  `public`, or `private` when the MCP endpoint requires authentication (`App::protect`).
- `resources/read`: derived from the inner response's `Cache-Control`. `max-age` gives `ttlMs`,
  and `public` / `private` gives `cacheScope`. Without the header the result is `ttlMs: 0` and
  `private`: the data may change and may depend on the caller. A handler that wants resources
  cached says so the HTTP way.

## Auth

- Route-level auth works as is, because the inner request carries the transport's headers.
- `App::protect` applies to `/mcp` like any other route: a protected app requires the token on
  the first request (`initialize` for legacy clients, any request for modern ones), which is what
  MCP clients expect before starting OAuth. Without `protect`, discovery and the list methods are
  open and the `401` arrives at the first call that needs it, propagated as described in
  Dispatch.
- With the `oidc` feature and `App::oidc`, lesto also serves
  `/.well-known/oauth-protected-resource` (RFC 9728, required by the MCP authorization spec in
  both eras): `authorization_servers` is the issuer, `resource` is `Mcp::resource_url(..)` or is
  derived from the `Host` header, and the `401`s of the MCP endpoint carry
  `resource_metadata="..."` in `WWW-Authenticate`. The authorization changes of 2026-07-28 (the
  `iss` check, Client ID Metadata Documents, `application_type`) concern clients and
  authorization servers, not the resource server.
- `tools/list` lists every exposed tool regardless of the caller's token. Filtering by permission
  depends on the separate roadmap item "documenting per-operation permissions as OpenAPI scopes".

## Code layout

- `crates/lesto/src/route.rs`: `McpExpose` on `RouteMeta` (the macro option's runtime value).
- `crates/lesto/src/mcp/` (phase 1 as built)
  - `mod.rs`: `Mcp`, `McpCall`, the `/mcp` handler, era selection, discover/initialize, the
    lists, `tools/call`, `resources/read`, `prompts/get`
  - `protocol.rs`: JSON-RPC envelope, `Era`, header validation, `_meta`, the per-era result
    shapes and error codes; if it grows past one reader's worth, it splits into `modern.rs` and
    `legacy.rs`
  - `catalog.rs`: operations to tools, resources and prompts; names per kind; schema assembly;
    resource template matching
  - `dispatch.rs`: arguments (or a resource URI) to request, response to result, resource
    caching fields
  - `prompt.rs`: `Prompt`
- `crates/lesto-macros`: the `mcp` option in `RouteArgs`, the compile-time checks, and
  `tests/ui/mcp_*.rs` cases for each diagnostic.
- `crates/lesto/tests/mcp.rs`, all through `oneshot`:
  - modern: `server/discover`, lists, calls, header mismatches, unsupported version, `404` on an
    unknown method;
  - legacy: `initialize` negotiation, `ping`, the same calls with legacy shapes;
  - `Mcp::legacy(false)` refusing `initialize` with the supported versions in the message;
  - `422` as `isError`, `401` propagated to the transport, Origin checks, caching fields,
    resources (lists, read, not found per era, `Cache-Control`), prompts (list, get, `4xx` as
    invalid params).
- `crates/lesto/tests/mcp_rmcp.rs`: interoperability with the official client. An app is served
  on `127.0.0.1:0` (as `tests/oidc.rs` does for its provider) and `rmcp`'s Streamable HTTP client
  connects once with `ClientLifecycleMode::Discover` (modern) and once with
  `ClientLifecycleMode::Initialize` (legacy), lists the tools, calls one successfully,
  calls one that fails validation (`isError`), and one that answers `404` (`isError`), lists
  the resource templates, reads a resource and gets a prompt. It runs in the default
  `cargo test --workspace`: no Docker, no network beyond loopback.
- `examples/02-notes`: every route but `delete` marked for MCP: four tools, a resource template
  (`note_text`) and a prompt (`tidy_note`).
- `scripts/mcp-inspector.sh`, run in CI after the tests: `examples/02-notes` against the MCP
  Inspector's CLI (pinned version, TypeScript SDK) in both eras: `tools/list --strict`, a call, a
  resource read, a prompt, a tool error, the `401`. A second client next to `rmcp`, stricter about schemas.
- Tutorial chapter 16, "MCP", with its snippets in `examples/99-tutorial`; `README.md` (attribute
  options, roadmap); `AGENTS.md` (layout, design decisions, roadmap entry removed).
- `cargo check -p lesto --no-default-features --features mcp` added to the per-feature checks.

## Phases

1. Tools in both eras: catalog, era selection, modern header validation, `server/discover`,
   legacy `initialize`, header forwarding, `401`/`403` propagation, Origin check, tests
   (including the `rmcp` interoperability test), tutorial chapter.
2. Resources and prompts, with the caching fields derived from `Cache-Control`.
3. RFC 9728 protected resource metadata with `oidc`.

## Open decisions

1. **Selection**: per-route opt-in only, or also `Mcp::tags([..])` to expose every route carrying
   one of the given tags? Recommendation: per-route only in phase 1; tags can be added later
   without breaking anything.
2. **Argument shape**: flat (`{"id": 1, "title": ".."}`, body properties merged with parameters)
   or structured (`{"path": {..}, "query": {..}, "body": {..}}`)? Recommendation: flat, which is
   easier for models and is what fastapi-mcp does; the `body` fallback covers collisions.
3. **Scope of the first change**: phase 1 alone, or all three phases together? Recommendation:
   phase 1 alone.
4. **Legacy versions**: 2025-11-25 and 2025-06-18, or also 2025-03-26 (no `MCP-Protocol-Version`
   header, which the spec lets a server read as 2025-03-26)? Recommendation: not 2025-03-26;
   clients that old are unlikely to matter, and accepting a missing header weakens era selection.
5. **When to drop legacy**: recommendation is to revisit once the major clients are confirmed on
   2026-07-28, and to keep `Mcp::legacy(false)` available from the start.

## References

- [MCP 2026-07-28: Key Changes](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
- [MCP 2026-07-28: Versioning and Compatibility](https://modelcontextprotocol.io/specification/2026-07-28/basic/lifecycle)
- [MCP 2026-07-28: Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
- [MCP 2026-07-28: Discovery](https://modelcontextprotocol.io/specification/2026-07-28/server/discover)
- [MCP 2025-11-25: Transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [The 2026-07-28 Specification (MCP blog)](https://blog.modelcontextprotocol.io/posts/2026-07-28/)
- [rmcp, the official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)

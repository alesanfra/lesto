# lesto

A Rust web framework with **FastAPI**'s ergonomics, built on [axum](https://github.com/tokio-rs/axum) 0.8.

**Batteries included, like FastAPI.** One line in `Cargo.toml` gives an API validation, an
OpenAPI document derived from the handler types, RFC 9457 errors, authentication, a database
layer with permissions, OpenTelemetry, CORS, timeouts and a dev server that reloads. Everything
is configured by values and environment variables, and all of it stays plain axum underneath:
a lesto app is an `axum::Router`, and an axum router can be mounted in a lesto app.

```toml
[dependencies]
lesto = "0.1"   # not on crates.io yet: path = "../lesto/crates/lesto"; features: sqlite, postgres, lambda, otel, oidc, mcp
```

| | lesto | axum + utoipa / aide | poem-openapi | dropshot | loco.rs |
|---|---|---|---|---|---|
| Built on | axum | axum | poem | hyper | axum |
| OpenAPI | from handler types, no annotations | utoipa: annotations; aide: from types | from types | from types, spec-first | via utoipa |
| Request validation | built in (garde) | third party | built in | via types | third party |
| Error format | RFC 9457 | your own | typed enums | own shape | framework-defined |
| Data layer | sqlx stores, permissions, RLS | — | — | — | SeaORM, full stack |
| Observability | OTel traces, logs, metrics | — | — | — | — |

lesto is for JSON APIs: API-first like poem-openapi and dropshot, with batteries like loco.rs,
on axum.

- A route is an annotated `async fn`: `#[lesto::get("/users/{id}")]`.
- JSON bodies and query strings are deserialized **and validated** with [garde](https://github.com/jprochazk/garde).
- Errors follow **RFC 9457 Problem Details** (`application/problem+json`): `type`, `title`, `status`,
  `detail`, `instance`, plus an `errors` extension with a JSON Pointer for every failed check.
  Unknown routes (`404`), wrong methods (`405`) and panicking handlers (`500`) answer the same
  way. One format, no switch: a client that needs another shape gets it from a layer of its own.
- The **OpenAPI 3.1** document is derived from the argument and return types via
  [schemars](https://github.com/GREsau/schemars), with no extra annotations. Scalar at `/docs`,
  Swagger UI at `/swagger` (pinned versions with integrity hashes), JSON at `/openapi.json`.
  Offline or behind a firewall, `App::scalar_script_url(..)` and `App::swagger_ui_base_url(..)`
  point the pages at your own copy.
- **One model, many views**: `#[lesto::model(views(Create(author, text), Update(text?)))]` on a
  struct derives serde, schemars and garde through lesto (no extra dependencies) and generates
  `NoteCreate` and `NoteUpdate` with the same attributes, plus `apply_*` methods.
- **Authentication** with self-documenting extractors: `Bearer`, `Basic`, `ApiKey<S>` land in
  `components.securitySchemes` and in the operation's `security` (the docs pages show the *Authorize* button).
  With the `oidc` feature, bearer JWTs are **verified** against an OpenID Connect provider: give
  `Oidc::discover(url)` the discovery URL (plus the accepted audiences and clients, optionally)
  and a `Jwt<Claims>` argument is a token whose signature (JWKS), algorithm, issuer, audience,
  client and lifetime were checked before the handler runs. `App::protect(auth.scopes(["admin"]))`
  guards a whole group of routes in one line; keys rotate by themselves, with refetches rate
  limited; `Oidc::from_env()` reads `LESTO_OIDC_*`.
- **Databases** with the `db` feature (sqlx; pick `postgres`, `mysql` or `sqlite`): a handler argument such as
  `NoteStore<ReadWrite, User>` is a store that opens one transaction per method, derives the
  principal from the token through a single `Authenticated` impl, and checks string permissions
  (`"notes:write"`) before each query. Read-only stores, `Public` principals and permission checks
  are enforced by the compiler where possible. For Postgres row level security, a principal's
  `transaction_settings` publish its identity (`SET LOCAL app.user_id = ...`) in the same round
  trip as the `BEGIN`, so a policy can read it and the query needs no `WHERE` of its own.
  `read_with` / `write_with` take an `Isolation` (`Snapshot`, `Serializable`) for the methods
  whose check has to hold until commit, and retry a conflict before answering 409 with
  `Retry-After` instead of 500. `#[store(read = "..", write = "..")]` declares the permission
  pair on the type, so the generated `read`/`write` have no requirement argument to get wrong.
- **MCP for AI agents** with the `mcp` feature: `mcp = "tool"` on a route and
  `App::mcp(Mcp::new())` serve it at `/mcp` as a Model Context Protocol tool, described from the
  route (doc comment, parameters and body as arguments, the response as output schema, hints from
  the method). A call runs the route itself, so validation, authentication and tracing are the
  route's own: a `422` reaches the model as a tool error it can correct, a `401` reaches the
  client as a `401`. `mcp = "resource"` makes a `GET` route a resource (a URI template when the
  path has parameters, cached as its `Cache-Control` says), `mcp = "prompt"` a prompt built by a
  route returning `lesto::mcp::Prompt`. MCP 2026-07-28 (stateless) and the 2025 revisions, with
  no session.
- **AWS Lambda** with the `lambda` feature: `lesto::lambda::serve(app)` runs the
  Lambda runtime inside Lambda (API Gateway REST and HTTP APIs, Function URLs, ALB) and a plain
  server anywhere else; REST stages are stripped, docs pages work behind them, and
  `lesto::lambda::test::invoke` replays event fixtures in tests.
- **Observability by default**: every request runs in a `tracing` span named `{method} {http.route}`
  whose fields are the [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/http/http-spans/)
  for HTTP servers, and every store transaction in a client span with the database conventions.
  With the `otel` feature (traces, logs and the `http.server.request.duration` metric), setup is
  environment only — `OTEL_EXPORTER_OTLP_ENDPOINT` and friends:
  `App::serve` installs the console subscriber and the OTLP export of **traces and logs** (every
  `tracing` event becomes a log record carrying its `trace_id`), flushes on shutdown, and
  continues a trace started upstream (`traceparent`). It stands aside if you install your own
  subscriber. `App::trace(Trace::new()...)` opts `url.query` in, trusts forwarding headers, or
  turns the span off.
- **Production defaults**: `App::serve` shuts down gracefully on `SIGTERM`/`Ctrl-C` (in-flight
  requests get 30 s, `App::shutdown_timeout`); `App::timeout` and `App::body_limit` (2 MB unless
  changed) answer `503` and `413` problems; `App::cors`, `App::compression` and
  `App::request_id` are one call each; credentials
  are redacted from `Debug` output, internal errors never leak their cause to the client.
- axum stays underneath: `State`, `Extension`, tower layers and `into_router()` work as always.
  Nobody is locked in: [appendix D](https://github.com/alesanfra/lesto/blob/main/docs/tutorial/D-leaving-lesto.md) shows the way out, step by
  step, and `App::merge` the way in from an existing axum router.

**Getting started**: install the CLI (`cargo install lesto-cli`, or `cargo install --path
crates/lesto-cli` from a checkout until it is published), then `lesto dev` in your project. It
builds, runs, and rebuilds + restarts on every save, keeping the port open while the code compiles
(like `fastapi dev`). `lesto run` does it once; `lesto openapi` prints the OpenAPI document without serving
(`-o api.json` writes it to a file).

**New here?** Start with the [tutorial](https://github.com/alesanfra/lesto/blob/main/docs/tutorial/README.md): sixteen short chapters, from
"Hello" to security, testing, databases, AWS Lambda, OpenTelemetry and MCP, with full code and `curl`
commands for every step.
The crates are not on crates.io yet: depend on them by `path` as the tutorial shows.

## Example

```rust
use lesto::prelude::*;

#[lesto::model]
struct CreateUser {
    /// Display name.
    #[garde(length(min = 1, max = 64))]
    name: String,
    #[garde(email)]
    email: String,
}

#[lesto::model]
struct User { id: u64, name: String, email: String }

/// Create a user.
///
/// The first line of the doc comment is the `summary`, the rest the `description`.
#[lesto::post("/users", status = 201, tag = "users", responses(409))]
async fn create_user(State(db): State<Db>, Json(body): Json<CreateUser>) -> Result<Json<User>, HttpError> {
    // ...
    Err(HttpError::conflict("email already taken"))
}

/// Fetch a user.
#[lesto::get("/users/{id}", tag = "users", responses(404))]
async fn get_user(Path(id): Path<u64>) -> Result<Json<User>, HttpError> {
    Err(HttpError::not_found(format!("user {id} not found")))
}

#[lesto::main]
async fn main() -> std::io::Result<()> {
    App::new()
        .title("Users API")
        .version("1.0.0")
        .routes(routes![create_user, get_user])
        .with_state(Db::default())
        .serve()                    // LESTO_HOST:LESTO_PORT, default 127.0.0.1:8000
        .await
}
```

Your project's `Cargo.toml`, all of it (serde, schemars, garde, tokio and tracing come through
lesto; add sqlx only to derive `sqlx::FromRow`):

```toml
[dependencies]
lesto = { path = "../lesto/crates/lesto" }   # features = ["sqlite", "lambda"] as needed
```

Examples: `LESTO_PORT=8765 cargo run -p hello` (smallest app), `-p notes` (CRUD on SQLite), `-p lambda`
(AWS Lambda); then open <http://127.0.0.1:8765/docs>.

## What the framework does

| FastAPI | lesto |
|---|---|
| `@app.post("/users", status_code=201, tags=["users"])` | `#[lesto::post("/users", status = 201, tag = "users")]` |
| `body: CreateUser` (pydantic) | `Json(body): Json<CreateUser>` (serde + garde) |
| `q: int = Query(ge=1)` | `Query(q): Query<ListQuery>` with `#[garde(range(min = 1))]` |
| `tags: list[str] = Query([])` | `#[serde(default)] tags: Vec<String>` in `Query<T>` (`?tags=a&tags=b`) |
| `id: int` path param | `Path(id): Path<u64>` |
| `raise HTTPException(404, "...")` | `Err(HttpError::not_found("..."))` (RFC 9457 response) |
| `response_model=User` | return type `Json<User>` |
| docstring → summary/description | `///` doc comment → summary/description |
| `app.include_router(r, prefix="/api")` | `app.nest("/api", other_app)` |
| `token: str = Depends(OAuth2PasswordBearer(...))` | `auth: Bearer` / `auth: Bearer<MyOAuth2>` |
| `key: str = Security(APIKeyHeader(name="X-API-Key"))` | `key: ApiKey<MyKey>` |
| `creds = Depends(HTTPBasic())` | `creds: Basic` |
| `jwt.decode(token, jwks_key, audience=..., issuer=...)` in a dependency | `token: Jwt<Claims>` + `App::oidc(..)` (feature `oidc`) |
| `/docs`, `/redoc`, `/openapi.json` | `/docs` (Scalar), `/swagger` (Swagger UI), `/openapi.json`; disable with `docs_url(None)` etc. |

### Attribute options

`#[lesto::get("/path", status = 200, tag = "x", tags("a", "b"), summary = "...", description = "...", operation_id = "...", deprecated, responses(404, 409), security("bearerAuth"), public, state = AppState, mcp = "tool" | "resource" | "prompt")]`

Available: `get`, `post`, `put`, `patch`, `delete`, `head`, `options`.

`status = N` rewrites every `200` of the handler; to put the status in the type instead, return
`Created<T>` (201), `Accepted<T>` (202) or `NoContent` (204), documented under that status.

`state = T` only affects the compile-time checks the macro emits (otherwise the state comes from a `State<T>` argument or from the `App<S>` the `routes![]` set is added to); see `docs/tutorial/B-common-problems.md`.
The default `operation_id` is `{function}_{path}_{method}` (`get_user_users__id__get`), unique per route as in FastAPI.

`mcp = "tool"` (feature `mcp`) exposes the operation as an MCP tool named after the function;
`mcp(tool, name = "search_notes")` names it. `mcp = "resource"` (a `GET` route) serves it as a
resource at `lesto://{title}{path}`, `mcp = "prompt"` (a `GET` route returning
`lesto::mcp::Prompt`) as a prompt whose arguments are the path and query parameters; both take
`name = ".."` too. It takes effect once the app calls `App::mcp`; see the tutorial, chapter 16.

Contributing or driving an agent? Read [AGENTS.md](https://github.com/alesanfra/lesto/blob/main/AGENTS.md).

### Views of a model

```rust
#[lesto::model(views(Create(author, text), Update(text?)))]  // serde + schemars + garde derives
struct Note { #[garde(skip)] id: u64, #[garde(length(min = 1))] author: String, #[garde(length(min = 1))] text: String }
// generates NoteCreate { author, text }, NoteUpdate { text: Option<String> },
// Note::apply_create(&mut self, NoteCreate), Note::apply_update(&mut self, NoteUpdate)
```

`field?` makes the field `Option<T>` (serde default, garde rules wrapped in `inner(..)`). Views copy the std,
serde, schemars and garde derives and attributes only (`sqlx::FromRow` and `#[sqlx(..)]` stay on the model). See the tutorial, chapter 6.

### Without macros

```rust
App::new().route(lesto::get("/ping").summary("Ping"), || async { "pong" })
```

Documentation is still derived from the types: `App::route` and `RouteSet::add` are generic over
`H: Handler<T, S> + OperationHandler<I, O>`, where `I` is the argument tuple (each `OperationInput`)
and `O` the return type (`OperationOutput`).

### Documented extractors and return types

Input: `lesto::Json<T>`, `lesto::Query<T>`, `lesto::Path<T>` (validated), `axum::Json/Query/Path` (docs only),
`State`, `Extension`, `HeaderMap`, `Method`, `Uri`, `Request`, `Bytes`, `String`, `Option<T>`, `Result<T, _>`.

Output: `Json<T>`, `String`, `&'static str`, `()`, `Html<T>`, `Result<T, E>`, `Option<T>`, `(StatusCode, T)`,
`(HeaderMap, T)`, `HttpError`, `StatusCode`, `Response`, `Redirect`.

To document your own extractor or response type, implement `OperationInput` / `OperationOutput`.
An extractor without `OperationInput` is accepted and left out of the document; the `strict-docs`
feature makes that a compile error. A response type always needs `OperationOutput`.

### Validation

`Json<T>` and `Query<T>` require `T: DeserializeOwned + garde::Validate`. Every field needs a
`#[garde(...)]` rule or `#[garde(skip)]`; alternatively `#[garde(allow_unvalidated)]` on the struct.
schemars reads the same garde attributes, so `length`, `range`, `pattern` etc. also show up in the
OpenAPI schema (`minLength`, `maximum`, ...).

JSON keys keep declaration order, in the OpenAPI document and in response bodies: struct fields,
problem extension members and `serde_json::Map`s built in order all come out in that order. lesto
turns on `serde_json`'s `preserve_order` for this, which is a unified Cargo feature: every
`serde_json::Map` in your build becomes insertion-ordered. A `HashMap` field has no order to keep;
use `IndexMap` or `BTreeMap` when the order matters.

### Errors (RFC 9457)

Both `HttpError` and validation `422`s respond with `application/problem+json`. `instance` is filled
automatically with the request path; `type` is `about:blank` and `title` the HTTP reason phrase unless
overridden with `with_type` / `with_title`. `with_extension` adds extension members.

```json
// HttpError::not_found("todo 99 not found")
{"type": "about:blank", "title": "Not Found", "status": 404, "detail": "todo 99 not found", "instance": "/todos/99"}

// 422: deserialization or garde, one entry per failed check (RFC 6901 pointer, "" = whole document)
{
  "type": "about:blank", "title": "Unprocessable Entity", "status": 422,
  "detail": "2 validation errors", "instance": "/users",
  "errors": [
    {"in": "body", "pointer": "/addresses/1/city", "detail": "length is lower than 1", "code": "value_error"},
    {"in": "body", "pointer": "/email", "detail": "missing field `email`", "code": "missing"}
  ]
}
```

### Authentication and security schemes

The extractors in `lesto::security` extract credentials **and** register the OpenAPI scheme. Token
verification stays with the handler or a middleware, except for OpenID Connect JWTs, which
`lesto::oidc::Jwt` (feature `oidc`) verifies. Missing or malformed credentials → `401` problem+json
with a `WWW-Authenticate` header. See the tutorial's [Security](https://github.com/alesanfra/lesto/blob/main/docs/tutorial/09-security.md) chapter.

## Layout

- `crates/lesto/` — the library: `App`, extractors, errors, OpenAPI model, `OperationInput`/`OperationOutput` traits;
  `lesto::db` (feature `db`): `Store<M, P, DB>`, `Authenticated`, `Public`, `TransactionSettings`,
  `Isolation`, `lesto::db::Error`;
  `lesto::lambda` (feature `lambda`): `serve`, `Options`, `test::invoke`;
  `lesto::oidc` (feature `oidc`): `Oidc`, `Jwt<C>`, `StandardClaims`, `Protect`;
  `lesto::mcp` (feature `mcp`): `Mcp`, `McpCall`, `Prompt`;
  `lesto::trace`: the request span and `Trace`; `lesto::otel` (feature `otel`): OTLP export of
  traces and logs plus trace context propagation, configured by the `OTEL_*` variables;
  `lesto::layers`: the tower layers `into_router` installs (`ProblemLayer`, `CatchPanicLayer`,
  `RequestSpanLayer`), usable on a plain `axum::Router`.
- `crates/lesto-macros/` — `#[lesto::get(...)]` attributes and friends, `#[lesto::views]`, `#[derive(Store)]`.
- `crates/lesto-cli/` — the `lesto` command: `dev` (watch, rebuild, restart with the socket kept open), `run`.
- `examples/01-hello/` — the smallest app. `examples/02-notes/` — full CRUD on SQLite with `lesto::db`,
  split into modules, also served to agents as MCP tools, a resource and a prompt. `examples/03-lambda/` — the same kind of API on AWS Lambda.
  `examples/04-opentelemetry/` — traces and logs in a local Jaeger or OpenObserve, `docker compose` included.
  `examples/05-routers/` — two APIs in one service (`/api/app/v1`, `/api/analytics/v1`), mounted with `nest`.
  `examples/06-oidc/` — bearer JWTs from a real OpenID Connect provider in Docker, `App::protect` included.
  `examples/99-tutorial/` — every tutorial snippet, compiled and tested.
- `docs/tutorial/` — the tutorial; `docs/build.sh` builds it as an mdBook site.

Design decisions, internals and the roadmap are in [AGENTS.md](https://github.com/alesanfra/lesto/blob/main/AGENTS.md).

## Performance

What lesto costs over the `axum::Router` it builds on, per request, measured through
`tower::Service` calls with no networking (`crates/lesto/benches/overhead.rs`, `cargo bench -p
lesto`; an Apple laptop, best of five rounds of 200,000 requests):

| Request | axum 0.8 | lesto |
|---|---:|---:|
| `GET /hello` | 616 ns | 911 ns |
| `POST /items`, JSON body validated, `status = 201` | 1,187 ns | 1,498 ns |
| `GET /missing` → `404` problem+json | 388 ns | 1,260 ns |
| `GET` a handler returning `HttpError::not_found` | 805 ns | 1,215 ns |

The difference is what lesto adds: the request span, the panic catcher, and an RFC 9457 body
where axum answers with none. A `422` costs more again (about 3.0 µs) because it also runs
garde and reports every failed check with its location — plain axum does not validate at all,
so there is nothing to compare it with.

`Trace::off()` is not on that list on purpose: it saves about 13 ns, because it is a branch
inside a layer that is installed anyway. Use it when you do not want a request span, not when
you want speed. `scripts/bench-http.sh` runs the same application behind `oha` over a real
socket, where the numbers above are lost in the noise of the network.

Build time is the other cost, and the reason `otel` is not a default feature. `examples/02-notes`
(`sqlite`), measured 2026-09-23 on an Apple Silicon laptop with the workspace's dev profile
(dependencies at `opt-level = 3`):

| | crates | clean build | rebuild after editing the app |
|---|---|---|---|
| without `otel` | 177 | 94 s | 1.0 s |
| with `otel` | 210 | 118 s | 1.1 s |

The OpenTelemetry SDK, the OTLP exporter and its HTTP client add 33 crates and about a quarter
to a clean build; a rebuild of your own code does not notice. Turn it on where telemetry is
exported, typically in the deployed build.

## Versioning

lesto follows semver. Public types that are expected to grow (`lesto::db::Error`, `Isolation`,
`lambda::Options`, `otel::Config`, the OpenAPI model) are `#[non_exhaustive]`, so a new variant
or field is not a breaking change; match them with a `_` arm and build them through their
constructors.

The crates lesto re-exports are part of its public API: `axum` (and `lesto::http`), `garde`,
`schemars`, `serde`, `serde_json`, `tokio`, `tracing`, tower-http's CORS types (`lesto::cors`), and
with the `db` feature `sqlx` and `uuid` (`lesto::db::sqlx`,
`lesto::db::uuid`). lesto 0.1 tracks axum 0.8; a new major (or 0.x minor) of any of them is a new
major (0.x minor) of lesto. Use the re-exports rather than a second direct dependency and the two
can never disagree.

MSRV: Rust 1.94, set by `sqlx` 0.9. Raising it is a minor release.

Changes are listed in [CHANGELOG.md](https://github.com/alesanfra/lesto/blob/main/CHANGELOG.md).

## Development

Rust 1.94 or newer (edition 2024; `sqlx` 0.9 sets the floor).

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo bench -p lesto                                 # per-request overhead against plain axum
cargo deny check                                     # licenses, advisories (cargo install cargo-deny)
cargo run -p lesto-cli -- dev -p notes --port 8765   # the CLI from this checkout
```

Features of `lesto`: `email`, `url` (garde rules) and `compression` (gzip), on by default; `pattern`; `db` + `postgres`/`mysql`/`sqlite`, `lambda`, `otel`, `oidc`, `mcp`, `anyhow`. Tests always run with all of them.

CI (`.github/workflows/ci.yml`) runs fmt, clippy and rustdoc with warnings denied, each feature of
`lesto` alone (`cargo hack`), the mdBook build and `cargo deny`; the tests on stable, including the
PostgreSQL row level security tests against a `postgres:18` service and the OpenID Connect tests
against a mock-oauth2-server one, then the MCP endpoint against the MCP Inspector's CLI; and `cargo check` on the MSRV (1.94).

## Roadmap

Bigger items, easiest first:

1. **Lambda adapter tested end to end on [floci](https://floci.io/)**: deploy the `lambda`
   example to floci's local AWS emulator (Lambda + API Gateway, a LocalStack drop-in that runs
   as a native binary, MIT) in CI, and call it over HTTP instead of only replaying event fixtures.
2. **MCP protected resource metadata**: RFC 9728's `/.well-known/oauth-protected-resource` with
   `oidc`, so MCP clients find the authorization server on their own. Tools, resources and
   prompts are done; this is phase 3 of [docs/design/mcp.md](https://github.com/alesanfra/lesto/blob/main/docs/design/mcp.md).

Smaller items: crates.io publication, `lesto new`, `AnyOf`/`AllOf` permission requirements, per-operation
permissions as OpenAPI scopes, `--watch`/`--ignore` for `lesto dev`, exporting the OpenAPI document as
3.0 with `x-amazon-apigateway-integration` extensions to create an API Gateway from it.

Not planned: typed header/cookie parameters beyond API keys, forms and multipart, websockets,
an ORM, hot patching without a restart.

## License

Licensed under the [Apache License, Version 2.0](https://github.com/alesanfra/lesto/blob/main/LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
lesto by you, as defined in the Apache-2.0 license, shall be licensed as above, without any
additional terms or conditions.

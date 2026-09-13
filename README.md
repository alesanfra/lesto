# lesto

A Rust web micro-framework with **FastAPI**'s ergonomics, built on [axum](https://github.com/tokio-rs/axum) 0.8.

- A route is an annotated `async fn`: `#[lesto::get("/users/{id}")]`.
- JSON bodies and query strings are deserialized **and validated** with [garde](https://github.com/jprochazk/garde).
- Errors follow **RFC 9457 Problem Details** (`application/problem+json`): `type`, `title`, `status`,
  `detail`, `instance`, plus an `errors` extension with a JSON Pointer for every failed check.
  Unknown routes (`404`), wrong methods (`405`) and panicking handlers (`500`) answer the same
  way. FastAPI's format (`{"detail": ...}`) is available with `App::error_format(ErrorFormat::FastApi)`.
- The **OpenAPI 3.1** document is derived from the argument and return types via
  [schemars](https://github.com/GREsau/schemars), with no extra annotations. Scalar at `/docs`,
  Swagger UI at `/swagger` (pinned versions with integrity hashes, or your own mirror), JSON at
  `/openapi.json`.
- **One model, many views**: `#[lesto::views(Create(author, text), Update(text?))]` on a struct generates
  `NoteCreate` and `NoteUpdate` with the same serde/garde/schemars attributes, plus `apply_*` methods.
- **Authentication** with self-documenting extractors: `Bearer`, `Basic`, `ApiKey<S>` land in
  `components.securitySchemes` and in the operation's `security` (the docs pages show the *Authorize* button).
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
- **AWS Lambda** with the `lambda` feature: `lesto::lambda::serve(app)` runs the
  Lambda runtime inside Lambda (API Gateway REST and HTTP APIs, Function URLs, ALB) and a plain
  server anywhere else; REST stages are stripped, docs pages work behind them, and
  `lesto::lambda::test::invoke` replays event fixtures in tests.
- **Observability by default**: every request runs in a `tracing` span named `{method} {http.route}`
  whose fields are the [OpenTelemetry semantic conventions](https://opentelemetry.io/docs/specs/semconv/http/http-spans/)
  for HTTP servers, and every store transaction in a client span with the database conventions.
  With the `otel` feature, setup is environment only — `OTEL_EXPORTER_OTLP_ENDPOINT` and friends:
  `App::serve` installs the console subscriber and the OTLP export of **traces and logs** (every
  `tracing` event becomes a log record carrying its `trace_id`), flushes on shutdown, and
  continues a trace started upstream (`traceparent`). It stands aside if you install your own
  subscriber. `App::trace(Trace::new()...)` opts `url.query` in, trusts forwarding headers, or
  turns the span off.
- **Production defaults**: `App::serve` shuts down gracefully on `SIGTERM`/`Ctrl-C`, credentials
  are redacted from `Debug` output, internal errors never leak their cause to the client.
- axum stays underneath: `State`, `Extension`, tower layers and `into_router()` work as always.

**Getting started**: install the CLI (`cargo install lesto-cli`, or `cargo install --path
crates/lesto-cli` from a checkout until it is published), then `lesto dev` in your project. It
builds, runs, and rebuilds + restarts on every save, keeping the port open while the code compiles
(like `fastapi dev`). `lesto run` does it once; `lesto new` and `lesto openapi` are reserved.

**New here?** Start with the [tutorial](docs/tutorial/README.md): fifteen short chapters, from
"Hello" to security, testing, databases, AWS Lambda and OpenTelemetry, with full code and `curl`
commands for every step.
The crates are not on crates.io yet: depend on them by `path` as the tutorial shows.

## Example

```rust
use lesto::prelude::*;

#[derive(Deserialize, JsonSchema, Validate)]
struct CreateUser {
    /// Display name.
    #[garde(length(min = 1, max = 64))]
    name: String,
    #[garde(email)]
    email: String,
}

#[derive(Serialize, JsonSchema)]
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

#[tokio::main]
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

Your project's `Cargo.toml` (the derives need the crates as direct dependencies, like `serde`):

```toml
[dependencies]
lesto = { path = "../lesto/crates/lesto" }   # features = ["sqlite", "lambda"] as needed
serde = { version = "1", features = ["derive"] }
garde = { version = "0.23", features = ["derive", "email"] }
schemars = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
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
| `/docs`, `/redoc`, `/openapi.json` | `/docs` (Scalar), `/swagger` (Swagger UI), `/openapi.json`; disable with `docs_url(None)` etc. |

### Attribute options

`#[lesto::get("/path", status = 200, tag = "x", tags("a", "b"), summary = "...", description = "...", operation_id = "...", deprecated, responses(404, 409), security("bearerAuth"), public, state = AppState)]`

Available: `get`, `post`, `put`, `patch`, `delete`, `head`, `options`.

`state = T` only affects the compile-time checks the macro emits (otherwise the state comes from a `State<T>` argument or from the `App<S>` the `routes![]` set is added to); see `docs/tutorial/B-common-problems.md`.
The default `operation_id` is `{function}_{path}_{method}` (`get_user_users__id__get`), unique per route as in FastAPI.

Contributing or driving an agent? Read [AGENTS.md](AGENTS.md).

### Views of a model

```rust
#[lesto::views(Create(author, text), Update(text?))]   // above #[derive]
#[derive(Serialize, Deserialize, JsonSchema, Validate)]
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

To add your own extractor or response type, implement `OperationInput` / `OperationOutput`.

### Validation

`Json<T>` and `Query<T>` require `T: DeserializeOwned + garde::Validate`. Every field needs a
`#[garde(...)]` rule or `#[garde(skip)]`; alternatively `#[garde(allow_unvalidated)]` on the struct.
schemars reads the same garde attributes, so `length`, `range`, `pattern` etc. also show up in the
OpenAPI schema (`minLength`, `maximum`, ...).

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

For clients expecting FastAPI's format:

```rust
App::new().error_format(ErrorFormat::FastApi)   // {"detail": "..."} and {"detail": [{"loc","msg","type"}]}
```

### Authentication and security schemes

The extractors in `lesto::security` extract credentials **and** register the OpenAPI scheme. Token
verification stays with the handler or a middleware. Missing or malformed credentials → `401` problem+json
with a `WWW-Authenticate` header. See the tutorial's [Security](docs/tutorial/09-security.md) chapter.

## Layout

- `crates/lesto/` — the library: `App`, extractors, errors, OpenAPI model, `OperationInput`/`OperationOutput` traits;
  `lesto::db` (feature `db`): `Store<M, P, DB>`, `Authenticated`, `Public`, `TransactionSettings`,
  `Isolation`, `lesto::db::Error`;
  `lesto::lambda` (feature `lambda`): `serve`, `Options`, `test::invoke`;
  `lesto::trace`: the request span and `Trace`; `lesto::otel` (feature `otel`): OTLP export of
  traces and logs plus trace context propagation, configured by the `OTEL_*` variables.
- `crates/lesto-macros/` — `#[lesto::get(...)]` attributes and friends, `#[lesto::views]`, `#[derive(Store)]`.
- `crates/lesto-cli/` — the `lesto` command: `dev` (watch, rebuild, restart with the socket kept open), `run`.
- `examples/01-hello/` — the smallest app. `examples/02-notes/` — full CRUD on SQLite with `lesto::db`,
  split into modules. `examples/03-lambda/` — the same kind of API on AWS Lambda.
  `examples/04-opentelemetry/` — traces and logs in a local Jaeger or OpenObserve, `docker compose` included.
  `examples/99-tutorial/` — every tutorial snippet, compiled and tested.
- `docs/tutorial/` — the tutorial; `docs/build.sh` builds it as an mdBook site.

Design decisions, internals and the roadmap are in [AGENTS.md](AGENTS.md).

## Development

Rust 1.85 or newer (edition 2024).

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo deny check                                     # licenses, advisories (cargo install cargo-deny)
cargo run -p lesto-cli -- dev -p notes --port 8765   # the CLI from this checkout
```

Features of `lesto`: `db` + `postgres`/`mysql`/`sqlite`, `lambda`, `otel`, `anyhow`. Tests always run with all of them.

CI runs fmt, clippy, tests on stable and 1.85, rustdoc with warnings denied, and `cargo deny`.

## Roadmap

Bigger items, easiest first:

1. **Lambda adapter tested end to end on [floci](https://floci.io/)**: deploy the `lambda`
   example to floci's local AWS emulator (Lambda + API Gateway, a LocalStack drop-in that runs
   as a native binary, MIT) in CI, and call it over HTTP instead of only replaying event fixtures.
2. **OAuth2 / JWT validation by configuration**: give lesto a `discovery_url` plus the accepted
   `audience` or client ids, and `Bearer` tokens are verified (signature via JWKS, issuer,
   audience, expiry) before the handler runs. Built as the first piece of an easy middleware
   system, so that adding a check to a group of routes is one line.
3. **MCP from the route attribute**: `#[lesto::get("/notes/{id}", tag = "notes", mcp = "tool")]`
   (or `"prompt"`, `"resource"`) exposes the operation to AI agents as an MCP tool, prompt or
   resource, reusing the OpenAPI schemas lesto already derives. To be designed carefully before
   any code: naming, auth, which operations map to which MCP primitive.

Smaller items: crates.io publication, `lesto new`, `lesto openapi`, `Store::atomic` (several
statements in one explicit transaction), `AnyOf`/`AllOf` permission requirements, per-operation
permissions as OpenAPI scopes, `--watch`/`--ignore` for `lesto dev`, exporting the OpenAPI document as
3.0 with `x-amazon-apigateway-integration` extensions to create an API Gateway from it.

Not planned: typed header/cookie parameters beyond API keys, forms and multipart, websockets,
an ORM, hot patching without a restart.

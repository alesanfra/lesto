# AGENTS.md — working on lesto

Guidance for coding agents (and humans) contributing to this repository. The tutorial under
`docs/tutorial/` is the reference for *using* lesto; this file is about *changing* it.

## What this is

lesto is a FastAPI-style web framework for Rust on top of axum 0.8: `#[lesto::get("/path")]`
on an `async fn`, `Json<T>`/`Query<T>`/`Path<T>` extractors that validate with garde, OpenAPI
3.1 derived from handler argument and return types, RFC 9457 errors, Scalar at `/docs` and
Swagger UI at `/swagger`. The `db` feature adds sqlx stores with principals and permissions,
the `lambda` feature runs the app on AWS Lambda; `lesto-cli` adds `lesto dev`. One library crate
with features, like FastAPI's extras: `lesto-macros` is separate only because proc macros must
be, `lesto-cli` because it is a binary. The project was called *presto* until 2026-09-12 (the
crates.io name was taken).

## Layout

```
crates/lesto/             library
  src/app.rs              App<S>: router + OpenAPI accumulation, docs routes, error middleware, listener
  src/route.rs            RouteMeta, RouteInfo, RouteSet (routes!), status override
  src/operation.rs        OperationInput / OperationOutput / OperationHandler, OperationBuilder
  src/extract.rs          Json, Query, Path (validating extractors)
  src/security.rs         Bearer, Basic, ApiKey<S>, Security<S>, AuthScheme, ApiKeyScheme
  src/error.rs            HttpError, ValidationError, Problem (RFC 9457), ErrorFormat
  src/openapi.rs          hand-written OpenAPI 3.1 model (serde)
  src/docs.rs             Scalar / Swagger UI HTML (relative openapi.json link)
  src/lib.rs              re-exports, prelude, routes! macro, __private compile-time checks
  src/db/                 feature `db` (+ postgres/mysql/sqlite): sqlx stores
    mod.rs                re-exports, prelude
    store.rs              Store<M, P, DB>: read/write (requirement → transaction → closure), extractor
    principal.rs          Authenticated (user-implemented), Public, Principal<S>, PrincipalDocs
    requirement.rs        Requirement<P>: Anyone, &'static str permission
    mode.rs               ReadOnly / ReadWrite, sealed Mode / Writable
    dialect.rs            BEGIN / BEGIN_READ_ONLY per database, begin_statement (isolation + SET LOCAL)
    handle.rs             Db<DB>: primary pool + optional read replica, conflict retry budget
    error.rs              lesto::db::Error → 403/404/409/500 problems, ResultExt::internal
  src/lambda.rs           feature `lambda`: AWS Lambda adapter over lambda_http: serve (Lambda or
                          local), Options (keep_stage, a per-router request mapper), test::invoke
  tests/integration.rs    end-to-end tests via tower::ServiceExt::oneshot
  tests/db.rs             SQLite in-memory end-to-end for lesto::db
  tests/db_postgres.rs    RLS end to end; skipped unless LESTO_TEST_POSTGRES_URL is set. Also the
                          compiled home of the chapter 13 row level security snippets, which
                          examples/02-notes cannot host (it is SQLite)
  tests/lambda.rs         API Gateway v1/v2, Function URL and ALB event fixtures
  tests/listener.rs       LISTEN_FDS socket inheritance
  tests/shutdown.rs       graceful shutdown (serve_until) finishes in-flight requests
  tests/ui/*.rs           compile-fail cases; *.expected lists the diagnostic fragments lesto owns
                          (db_*.rs for lesto::db), checked by tests/ui.rs
crates/lesto-macros/      proc macros: #[lesto::get] and friends (RouteInfo marker type +
                          per-argument checks), #[lesto::views], #[derive(Store)] with the
                          optional #[store(read = .., write = ..)] permission pair with the
                          optional #[store(read = .., write = ..)] permission pair
crates/lesto-cli/         the `lesto` binary: dev (watch + rebuild + restart, socket kept open),
                          run, new/openapi (reserved, exit 2)
  src/cargo.rs            cargo metadata; cargo build --message-format=json → executable path
  src/process.rs          spawn with the socket on fd 3 (LISTEN_FDS), stop (SIGTERM, grace, SIGKILL)
  src/watch.rs            notify watcher, ignore filters, debounce
examples/01-hello/        package `hello`: one route, the smallest app
examples/02-notes/        package `notes`: full CRUD on SQLite with lesto::db, split into
                          lib.rs / state.rs / auth.rs / notes/{model,store,handlers}.rs, tests/api.rs
examples/03-lambda/       package `lambda`: chapter 14 (lesto::lambda), in-memory notes, event-fixture test
examples/99-tutorial/     package `tutorial`: every tutorial snippet, compiled and tested (keep in sync)
docs/tutorial/            the tutorial, mdBook (docs/book.toml, docs/build.sh → docs/book, gitignored)
```

## Commands

```sh
cargo test --workspace                        # unit + integration + tutorial + UI tests
cargo clippy --workspace --all-targets --all-features   # must be warning-free
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features   # intra-doc links must resolve
cargo deny check                              # licenses and advisories (deny.toml)
cargo check -p lesto --no-default-features --features db   # each feature alone must compile too
cargo test -p lesto --test ui                 # after changing a diagnostic message, update tests/ui/*.expected
sh docs/build.sh                              # needs `cargo install mdbook`
LESTO_PORT=8765 cargo run -p notes            # port 8000 may be taken on dev machines
docker run --rm -e POSTGRES_PASSWORD=lesto -p 5432:5432 postgres:18   # for tests/db_postgres.rs
LESTO_TEST_POSTGRES_URL=postgres://postgres:lesto@127.0.0.1:5432/postgres cargo test -p lesto --test db_postgres
cargo run -p lesto-cli -- dev -p notes --port 8765      # lesto dev from this checkout
```

Test, clippy, fmt, rustdoc and (if docs changed) the mdBook build must pass before a change is
done; `.github/workflows/ci.yml` runs the same on every push (tests on stable and on the MSRV,
1.85). Do not claim success without running them. Gate commits on the test result, never on "it
should pass". Run `git grep -i presto` before committing: the old name must not come back.

## Conventions

- **Language**: American English everywhere (code, comments, docs, commits). There is one
  tutorial and one README; do not add translations.
- **Docs and code stay in lockstep**: any snippet added to the tutorial goes into
  `examples/99-tutorial` (with a test if it makes a runtime claim). Any behavior change updates the
  tutorial chapter and `README.md`.
- **Dependencies** are declared once in `[workspace.dependencies]` and referenced with
  `.workspace = true`. Prefer no new dependencies. Anything heavy or niche goes behind a feature
  of `lesto` (`db`, `lambda`), not into a new crate; `lesto`'s dev-dependency on itself turns
  every feature on for tests, so `cargo test --workspace` covers them all.
- **Errors are RFC 9457** by default; never add an ad hoc error JSON shape. New error responses
  go through `HttpError` / `Problem`.
- **Everything documented from types**: a new extractor implements `FromRequestParts` +
  `OperationInput`; a new response type implements `IntoResponse` + `OperationOutput`. A new
  macro option is parsed in `lesto-macros` (`RouteArgs`), stored on `RouteMeta`, applied in
  `PendingOperation::operation`, documented in `README.md` "Attribute options" and in the tutorial.
- **Diagnostics are a feature**: traits that users can fail to implement carry
  `#[diagnostic::on_unimplemented]`; the macro emits per-argument checks (`__private`). When you
  change a message, update the `tests/ui/*.expected` fragments and read the output: the message
  must tell the reader what to do, not just what is wrong. Pin only the text lesto writes (its
  own messages and the span they point at), never rustc's surrounding boilerplate: that wording
  changes between compiler releases and an exact snapshot then fails with no lesto change.
- **No panics on user input** at runtime. Validation of macro options happens at compile time.
  A panic that happens anyway is caught by `into_router` and answered as a `500` problem.
- **Never mutate the environment at runtime.** `std::env::set_var`/`remove_var` are `unsafe`
  in edition 2024 for a reason (they race with `getenv` on other threads). Read variables; keep
  settings on the value they configure (`Options`, `App`), not in the process environment.
- **Secrets stay out of `Debug`**: types that hold a credential implement `Debug` by hand and
  print `[redacted]`. Internal error details (serializer messages, panic payloads, database
  errors) go to `tracing`, never into a response body.
- **Commits**: one logical change per commit, imperative subject, body explains why.

## Design decisions (and why)

Recorded here because they are not derivable from the code. Do not undo them casually.

- **axum, not actix-web.** Extractor traits, tower middleware ecosystem, `Handler<T, S>`
  exposing the argument tuple (which is what lets lesto document handlers with no annotations).
  Details in the tutorial, appendix C.
- **RFC 9457 by default, FastAPI's shape as an option.** A standard media type beats an ad hoc
  `{"detail": ...}`; FastAPI's format is kept as `ErrorFormat::FastApi` for clients that expect
  it, implemented as a response-rewriting middleware so handlers never know about it.
- **Documentation from types, lazily.** `RouteSet::add` requires
  `H: Handler<T, S> + OperationHandler<I, O>`; `I` (argument tuple) and `O` (return type) drive
  the OpenAPI document through `OperationInput`/`OperationOutput`. Operations are described in
  `App::openapi()`, so builder call order does not matter. The default `operationId` is also
  computed there, from the *final* path: `{fn}_{path}_{method}` (FastAPI's rule), so two `list`
  functions in different modules or nested apps never collide.
- **Docs assets are pinned.** `docs.rs` hard-codes the Scalar and Swagger UI versions and their
  SRI hashes (`SCALAR_VERSION`, `SWAGGER_UI_VERSION`). Bumping them means downloading the files,
  recomputing `sha384`, and updating the integration test. `App::scalar_script_url` /
  `swagger_ui_base_url` exist for mirrors; a custom URL carries no integrity attribute.
- **The route attribute leaves the function alone.** It emits a braced struct with the same
  name (type namespace only) implementing `RouteInfo`, so `routes![f]` finds both the fn and its
  metadata. Alternatives (renaming the fn, registering into a global) were rejected: the handler
  stays a plain function you can call in tests.
- **`#[lesto::views]` instead of `Note<View>` generics or hand-written DTOs.** One model with
  all attributes is the source of truth; generated `ModelCreate`/`ModelUpdate` keep handler
  signatures free of type parameters and produce ordinary OpenAPI schemas.
- **`lesto::db`: one transaction per store method,** opened and closed by `read`/`write`; no
  request-scoped transaction. Closure primitives rather than an attribute macro on methods: the
  flow stays visible and errors inside the body point at user code. Requirements are always
  explicit (`Anyone` or a permission string): forgetting one is impossible because it is an
  argument. `lesto::db::Error` is an enum, not `anyhow`, because responses and OpenAPI docs are
  derived from the type; `Error::internal` and the `anyhow` feature cover the "any other error"
  case. `Writable` is named so to avoid clashing with `std::io::Write` in glob imports.
- **lesto-cli hands the socket over instead of proxying.** `lesto dev` binds the port itself
  and passes the socket with systemd's `LISTEN_FDS` protocol, so the same binary works under
  `lesto dev`, systemd socket activation and `systemfd`, with no feature flag. True hot patching
  (`subsecond`, `hot-lib-reloader`) was rejected: invasive and incompatible with generics and
  macros. `[profile.dev.package."*"] opt-level = 3` makes the first build slower and rebuilds of
  workspace crates unaffected.
- **`lesto::lambda` wraps `lambda_http`, it does not reimplement it.** The official runtime already
  turns API Gateway v1/v2, Function URL and ALB events into `http::Request`s and accepts any
  tower service, so the crate adds only what lesto users need: the Lambda-or-local switch on
  `AWS_LAMBDA_RUNTIME_API`, stage stripping by default, and an in-process test helper. The
  stage is stripped by a `tower::ServiceExt::map_request` around the router (it must run
  *before* routing; a `Router::layer` runs after) rather than through the runtime's
  `AWS_LAMBDA_HTTP_IGNORE_STAGE_IN_PATH` variable, which is process-wide. Docs pages link
  `openapi.json` relatively for the same reason (a stage or proxy prefix the app does not know).

## How lesto works (the parts that are not obvious)

- `routes![f]` expands to `f::__lesto_check(&set); set.add(<f as RouteInfo>::meta(), f)`. The
  `where` clauses of the generated `__lesto_check<S>` are the per-argument extractor checks, so
  the state `S` is inferred from the `App<S>` the set is added to (no `state = ..` needed). With
  a `State<T>` argument or `state = T` the checks run immediately, on the argument's span. A
  missing attribute yields `expected type, found function`.
- A nested `/` route is documented at the prefix itself (that is where axum serves it).
- `HttpError::into_response` emits problem+json without `instance`; the middleware installed by
  `into_router` adds it and, with `ErrorFormat::FastApi`, rewrites the body. `into_router` also
  installs a `404` fallback (unless `App::fallback` was called), a `405`
  `method_not_allowed_fallback`, and `CatchPanicLayer` (inside the `instance` middleware, so the
  `500` gets its `instance` too). The panic catcher is hand-rolled (`catch_unwind` around
  `poll`) to avoid a `tower-http` dependency.
- `App::serve` → `serve_at` → `serve_on` → `serve_until(listener, shutdown)`;
  `shutdown_signal()` is `SIGTERM` or `Ctrl-C`. Graceful shutdown is `axum::serve(..)
  .with_graceful_shutdown`, nothing more.
- `status = N` wraps the handler (`WithStatus`) and rewrites a `200` into `N` at runtime.
- `App::serve()` binds `lesto::bind_address()`: `LESTO_HOST`/`LESTO_PORT`, `PORT` as fallback,
  default `127.0.0.1:8000`; `serve_at(addr)` is explicit. `lesto dev` sets `LESTO_HOST`,
  `LESTO_PORT` and `PORT` in the child.
- `App::serve` / `lesto::listener`: with `LISTEN_FDS=1` (and `LISTEN_PID` absent or equal to the
  pid) the socket on fd 3 is used and the address is ignored. The variables are *not* removed
  (see "never mutate the environment"); a `static AtomicBool` makes sure fd 3 is taken once.
  Examples that bind manually must call `lesto::listener(addr)` or they fail with "address
  already in use" under `lesto dev`. Unix only.
- `#[lesto::views(Create(a, b), Update(b?))]` copies the listed fields with all their attributes
  into `ModelCreate`/`ModelUpdate`; `?` → `Option<T>` + `serde(default)` + garde rules wrapped
  in `inner(..)` (schemars does not mirror `inner(..)` rules into the schema); also emits
  `Model::apply_<view>`. It must sit above `#[derive]` because views inherit what is below it.
- schemars runs with `preserve_order` and a transform replacing boolean schemas (Swagger UI
  cannot render `true`).
- **`lesto::db` coherence trick**: `Principal<S>` has a blanket impl for every `Authenticated`
  (with `S = <P as Authenticated>::State`) plus one for `Public` (any `S`). That compiles only
  because `Authenticated` has no type parameters and `State` is an associated type, not a trait
  parameter: no downstream crate can implement it for `Public`. `PrincipalDocs` is the `S`-free
  half so that `Store<M, P, DB>: OperationInput` (which has no `S`) can be written.
  `&'static str: Requirement<P>` only for `P: Authenticated` makes a permission on a `Public`
  store a compile error. `Store::read`/`write` take `for<'c> AsyncFnOnce(&'c mut DB::Connection)`
  (`read_with`/`write_with` an `AsyncFn`, see below); the future is `Send` through auto-trait
  leakage, no boxing. Inside a closure the error type is
  generic, so use `Error::not_found(..)` etc. rather than `.into()`. SQLite has no read-only
  transactions: `BEGIN DEFERRED`. In SQLite tests use `max_connections(1)`: every
  `sqlite::memory:` connection is a separate database.
- **`lesto::db` transaction settings are values, not SQL.** `SET LOCAL` accepts no bind
  parameters on any database, so a setting's value is interpolated; `Literal` is therefore a
  closed set (`bool`, integer, `Uuid`) whose text forms cannot contain a quote, with no text
  variant and no way to add one downstream, and names are checked `&'static str`. lesto builds
  the whole statement, so `BEGIN` is always first (sqlx then fails the call if the connection
  did not stay in a transaction) — the caller never formats SQL. The type is named
  `TransactionSettings`, not `SessionVars`: in Postgres "session" is the opposite scope of what
  `SET LOCAL` does, and a name suggesting session scope invites setting them per connection.
  They are read off the principal once, when the store is built, so `read`/`write` need no
  `P: PrincipalDocs` bound that every user's store `impl` block would have to repeat.
- **`Isolation` is a value, not a type parameter**, because it changes what the database does,
  not which methods compile (unlike `Mode`). It sits on `read_with`/`write_with` rather than as
  a fourth argument to `read`/`write`, so the common case pays nothing. Named `Isolation` even
  though SQLite has no isolation levels: SQLite only ever provides serializable, so the request
  is meaningful there and only the rendering differs (`BEGIN IMMEDIATE` takes the write lock up
  front instead of meeting `SQLITE_BUSY` halfway through a check-then-act). MySQL overrides
  `begin_statement` because `START TRANSACTION` takes no isolation clause: it has to precede the
  statement, the opposite order from Postgres.
- **A lost race is a 409, not a 500.** `40001`, `40P01` and SQLite's `SQLITE_BUSY` family answer
  409 with `Retry-After: 0`, because the request is safe to resend. Lock *timeouts*
  (`ER_LOCK_WAIT_TIMEOUT`, `55P03`) stay 500s on purpose: a retry does not fix one, and hiding
  them would hide a transaction somebody is holding too long.
- **Only `read_with`/`write_with` retry, and only they take an `AsyncFn`.** `read`/`write` keep
  `AsyncFnOnce` and duplicate the few lines rather than delegating, so the common case is not
  made to satisfy a bound it has no use for — at the database's own isolation there is no
  conflict to retry. The budget lives on `Db` (`with_conflict_retries`, default 2) because it is
  policy, not a per-call decision, and a fourth argument on every store method would be. A
  retried closure runs more than once: that is what `AsyncFn` announces, and why the docs say to
  keep outside-visible effects out of it. A test that synchronises attempts with a barrier must
  disable retrying or arrange for only the first attempt to wait, or it deadlocks.
- **The permission pair can live on the store type** (`#[store(read = .., write = ..)]`), which
  generates `read`/`write`/`read_with`/`write_with` with no requirement argument — a method then
  cannot name the wrong permission, without adding a domain type parameter to `Store` (which
  would break every `#[derive(Store)]` newtype). The derive reads `M`, `P` and `DB` out of the
  inner `Store<M, P, DB>` path so the generated signatures can name the connection type; that is
  why the inner type has to be spelled with all three arguments. Whatever is left undeclared
  keeps the explicit two-argument form, so one store can mix them. A read-only store declares no
  `write`: `Store::write` stays reachable through the `Deref`, a deliberate bypass rather than an
  accident, since it needs both `ReadWrite` and an explicit requirement.
- **No `Store::atomic`.** It would be `write_with` with another name: one transaction around
  several statements is what a closure already is. What was actually missing is the way to share
  a *query* between methods, which is the `Connection<DB>` alias plus the convention of writing
  `..._in(conn, ..)` helpers. Two public store methods sharing one transaction stays impossible
  on purpose — that is the boundary the design is built on.
- **`lesto::lambda`**: `Options::keep_stage` selects whether the request mapper strips
  `/{stage}` (read from the event's `RequestContext`, v1 and v2; `$default` has no prefix).
  `lambda_http` still *adds* the stage when the event path lacks it, so the mapper always sees
  it. `lambda_http` is built with only `apigw_rest`, `apigw_http`, `alb` (no websockets) to
  keep the binary small.
- **lesto-cli**: builds with `--message-format=json-render-diagnostics` (diagnostics to the
  terminal, artifact paths on stdout), spawns the executable directly (not `cargo run`) so it
  owns the process, `dup2`s the listener onto fd 3 in `pre_exec`, debounces watch events 300 ms,
  stops the child with SIGTERM (which `App::serve` handles gracefully), 2 s grace, SIGKILL. On
  Windows it only restarts.
- **`#[lesto::views]` copies an allow-list**, not everything: derives whose last path segment
  is in `VIEW_DERIVES` (std + serde + schemars + garde) and attributes in `VIEW_ATTRS`. Other
  crates' derives (`sqlx::FromRow`) and their field attributes are dropped. serde/garde
  attributes are parsed as `Meta`, never matched as strings.

## Common compile errors (see docs/tutorial/B-common-problems.md)

| Message | Fix |
|---|---|
| `lesto cannot document X as a handler argument` | derive `JsonSchema` on the payload and use `Json/Query/Path`, or `impl OperationInput for X {}` |
| `lesto cannot document X as a handler response` | return `Json<T>` (T: Serialize + JsonSchema), or `impl OperationOutput for X {}` |
| `X cannot be a handler argument in this position` | body extractor must be the last argument |
| `X is not an extractor` | payload type lacks `Deserialize`/`JsonSchema`/`Validate`, or the extractor expects a state other than the `App<S>` it is registered on (`state = AppState` pins it) |
| `expected type, found function f` in `routes![f]` | the handler is missing `#[lesto::get(...)]` |
| `cannot find garde/schemars in the crate root` | add them as direct dependencies of the user crate |
| `type annotations needed for App<_>` | write `App::<()>::new()` / `App::<AppState>::new()` |
| `this store is read-only: ReadOnly does not allow write` | declare `YourStore<ReadWrite, _>` or bound the impl with `M: Writable` |
| `Public cannot hold permissions` | pass `Anyone`, or use an `Authenticated` principal in the handler |
| `X is not a principal for state S` | `impl Authenticated for X` with `type State = S`, or use `Public` |
| `#[derive(Store)] expects a tuple struct` | `struct S<M, P>(lesto::db::Store<M, P, Db>);` |
| `type annotations needed` on `.into()` in a store closure | use `Error::not_found(..)`/`Error::conflict(..)`/`Error::http(e)` |

## Roadmap and non-goals

Bigger items, easiest first (mirrored in `README.md`, keep the two lists in sync):

1. Lambda adapter tested end to end on floci (<https://floci.io/>: local AWS emulator, Lambda +
   API Gateway, LocalStack drop-in on port 4566, native binary or Docker, MIT) in CI, on top of
   the event-fixture tests in `tests/lambda.rs`.
2. OAuth2 / JWT validation by configuration: `discovery_url` + accepted `audience` / client ids
   → `Bearer` tokens verified (JWKS signature, issuer, audience, expiry) before the handler
   runs. Design it as the first piece of an easy middleware system (one line to protect a group
   of routes); the OpenAPI security scheme should come out of the same configuration.
3. MCP from the route attribute: `mcp = "tool" | "prompt" | "resource"` on `#[lesto::get]` and
   friends exposes the operation to agents, reusing the derived JSON schemas. Needs a written
   design first (naming, auth, which operations map to which MCP primitive, transport).

Smaller items, in rough priority order: crates.io publication (publish order: `lesto-macros`,
`lesto`, `lesto-cli`; the manifests are ready); `lesto new`
(scaffold) and `lesto openapi` (print the document without serving); `Store::atomic` for several
statements in one explicit transaction; `AnyOf`/`AllOf` requirements; documenting per-operation
permissions as OpenAPI scopes; `--watch`/`--ignore` for `lesto dev`; a `strict_docs()` mode
that fails on undocumented types; exporting the OpenAPI document as 3.0 with
`x-amazon-apigateway-integration` extensions so an API Gateway can be created from it (API
Gateway imports 3.0 only).

Out of scope unless asked: actix-web backend, websockets, multipart, typed header/cookie
parameters beyond API keys, an ORM or query builder, role hierarchies or permission wildcards
(`has_permission` is user code), `sqlx::Any`, hot patching without restart.

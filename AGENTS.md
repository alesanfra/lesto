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
  src/error.rs            HttpError, ValidationError, Problem (RFC 9457), the render context
  src/openapi.rs          hand-written OpenAPI 3.1 model (serde)
  src/docs.rs             Scalar / Swagger UI HTML (relative openapi.json link)
  src/layers.rs           the tower layers into_router installs, public: ProblemLayer (renders
                          RFC 9457 through a task-local request context), CatchPanicLayer,
                          RequestSpanLayer, TimeoutLayer (opt-in, `App::timeout`). Hand-written
                          services, pin-projected futures
  src/trace.rs            request span (HTTP semconv), Trace config, feature `otel`: propagation
  src/otel.rs             feature `otel`: Config from the OTEL_* variables, init/init_named,
                          Telemetry guard (tracer + logger + meter providers), auto_init called by
                          App::serve_until, the tracing→OTLP logs bridge and its feedback filter
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
    trace.rs              the transaction's client span (database semconv)
  src/lambda.rs           feature `lambda`: AWS Lambda adapter over lambda_http: serve (Lambda or
                          local), Options (keep_stage, a per-router request mapper), test::invoke
  tests/integration.rs    end-to-end tests via tower::ServiceExt::oneshot
  benches/overhead.rs     per-request overhead against plain axum (harness = false, no dev-dep)
  tests/db.rs             SQLite in-memory end-to-end for lesto::db
  tests/db_postgres.rs    RLS end to end; skipped unless LESTO_TEST_POSTGRES_URL is set. Also the
                          compiled home of the chapter 13 row level security snippets, which
                          examples/02-notes cannot host (it is SQLite)
  src/metrics.rs          feature `otel`: http.server.request.duration, gated on an AtomicBool
  tests/trace.rs          the span fields, through a hand-rolled capturing tracing::Subscriber
  tests/metrics.rs        the duration histogram through the SDK's in-memory exporter
  tests/lambda.rs         API Gateway v1/v2, Function URL and ALB event fixtures
  tests/listener.rs       LISTEN_FDS socket inheritance
  tests/shutdown.rs       graceful shutdown (serve_until) finishes in-flight requests
  tests/ui/*.rs           compile-fail cases; *.expected lists the diagnostic fragments lesto owns
                          (db_*.rs for lesto::db), checked by tests/ui.rs
crates/lesto-macros/      proc macros: #[lesto::get] and friends (RouteInfo marker type +
                          per-argument checks), #[lesto::model], #[lesto::views],
                          #[lesto::main]/#[lesto::test]
                          (tokio's, through `lesto::tokio`), #[derive(Store)] with the
                          optional #[store(read = .., write = ..)] permission pair
  src/garde/              garde_derive 0.23.0, vendored (NOTICE.md): emits ::lesto::garde paths
crates/lesto-cli/         the `lesto` binary: dev (watch + rebuild + restart, socket kept open),
                          run, openapi (runs the app with LESTO_OPENAPI_PATH: App::serve writes
                          the document there and returns)
  src/cargo.rs            cargo metadata; cargo build --message-format=json → executable path
  src/process.rs          spawn with the socket on fd 3 (LISTEN_FDS), stop (SIGTERM, grace, SIGKILL)
  src/watch.rs            notify watcher, ignore filters, debounce
examples/01-hello/        package `hello`: one route, the smallest app
examples/02-notes/        package `notes`: full CRUD on SQLite with lesto::db, split into
                          lib.rs / state.rs / auth.rs / notes/{model,store,handlers}.rs, tests/api.rs
examples/03-lambda/       package `lambda`: chapter 14 (lesto::lambda), in-memory notes, event-fixture test
examples/04-opentelemetry/ package `opentelemetry-example` (not `opentelemetry`: that is the API
                          crate): chapter 15 end to end — compose.yaml with Jaeger (traces) and
                          OpenObserve (traces + logs), an unauthenticated SQLite API, verify.sh
                          (requests + a trace and a log search). Not run in CI: needs Docker
examples/05-routers/      package `routers`: chapter 10 as a running app, two APIs (`/api/app/v1`,
                          `/api/analytics/v1`) as FastAPI-style routers mounted with `nest`, tests/api.rs
examples/99-tutorial/     package `tutorial`: every tutorial snippet, compiled and tested (keep in sync;
                          appendix D's plain axum router too)
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
cargo check -p lesto --no-default-features --features otel
cargo test -p lesto --test ui                 # after changing a diagnostic message, update tests/ui/*.expected
sh docs/build.sh                              # needs `cargo install mdbook`
cargo bench -p lesto                          # overhead vs axum; LESTO_BENCH_ITERS/_ROUNDS shrink it
sh scripts/bench-http.sh                      # throughput over a socket (needs `oha`)
LESTO_PORT=8765 cargo run -p notes            # port 8000 may be taken on dev machines
docker run --rm -e POSTGRES_PASSWORD=lesto -p 5432:5432 postgres:18   # for tests/db_postgres.rs
(cd examples/04-opentelemetry && docker compose up -d openobserve && sh verify.sh)  # OTLP end to end
(cd examples/04-opentelemetry && docker compose up -d jaeger)        # traces only: OTEL_LOGS_EXPORTER=none OTEL_METRICS_EXPORTER=none
LESTO_TEST_POSTGRES_URL=postgres://postgres:lesto@127.0.0.1:5432/postgres cargo test -p lesto --test db_postgres
cargo run -p lesto-cli -- dev -p notes --port 8765      # lesto dev from this checkout
```

Test, clippy, fmt, rustdoc and (if docs changed) the mdBook build must pass before a change is
done; `.github/workflows/ci.yml` runs the same on every push and pull request, in three jobs
(lint, test with a Postgres service for `db_postgres`, `cargo check` on the MSRV 1.94, which
`sqlx` 0.9 dictates) built with `--profile ci` (unoptimized dependencies, no debug info). When
raising `rust-version`, change the MSRV job too. Do not claim success without running them. Gate commits on the test result, never on "it
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
- **RFC 9457, and only that.** A standard media type beats an ad hoc `{"detail": ...}`.
  FastAPI's shape was offered as `ErrorFormat::FastApi` until 2026-09-20 and was removed: two
  wire formats meant two OpenAPI shapes for every error response, a format argument threaded
  through `App`, `RouteSet`, `OperationBuilder` and the problem layer, and a second body to
  keep in step with the first. A client that needs another shape rewrites the body in a layer
  of its own, which is where a presentation concern belongs.
- **JSON keys keep declaration order, always** (decided 2026-09-23): in the OpenAPI document and
  in response bodies. That is `preserve_order` on `serde_json` and `schemars`. Without it
  `serde_json::Map` is a `BTreeMap`, so schemars hands over alphabetical properties and the
  order is gone before lesto sees it; problem extensions and `Json<Value>` bodies sort too. The
  cost is accepted: the feature is unified, so every `serde_json::Map` in a dependent's graph is
  an `IndexMap`. A separate crate would not scope it (unification is per graph). Tests read the
  raw bytes (`response_bodies_keep_key_order`): parsing into a `Value` would hide a regression.
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
- **`#[lesto::model]` and a vendored garde derive.** The four derives a model needs emit
  absolute paths, which made `serde`, `schemars` and `garde` direct dependencies of every
  application. serde and schemars take `crate = ".."`; `garde_derive` does not, so its source is
  vendored in `lesto-macros/src/garde` with `::garde` → `::lesto::garde` (decided 2026-09-23,
  over an upstream PR that would take a release cycle and a documented extra dependency). The
  vendored version must match `garde` in `[workspace.dependencies]`: the generated code calls
  its runtime API. A model with no `#[garde]` attribute gets `allow_unvalidated` (it has no
  rules); one rule anywhere restores garde's all-fields strictness, which is what catches a
  forgotten `dive`. `views(..)` is an option of `model` rather than a second attribute because
  `model` emits the derives, so the "views above derive" ordering rule disappears.
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
- **Logs go out with the traces.** `opentelemetry-appender-tracing` turns every `tracing` event
  into an OTLP log record with the `trace_id`/`span_id` of the span it happened in, which is what
  makes a backend line up the logs of a request with its trace; the console layer keeps printing
  them. The bridge layer carries a filter turning the OpenTelemetry crates, `reqwest`, `hyper`,
  `h2` and `tower` off (`no_feedback`): those crates report through `tracing`, so without it an
  export failure is logged, exported, and fails again. It compares whole crate names (the first
  `::` segment of the target), not `Targets` prefixes: `Targets` turned `opentelemetry` into
  "anything starting with opentelemetry" and dropped every log of `opentelemetry_example`. Signals are switched off one at a time
  with the spec's own `OTEL_TRACES_EXPORTER=none` / `OTEL_LOGS_EXPORTER=none` /
  `OTEL_METRICS_EXPORTER=none`.
- **One metric, recorded by the request span layer** (decided 2026-09-23): `http.server.request.duration`,
  the histogram the HTTP conventions require, with their bucket boundaries; rate, errors and
  latency all derive from it. Gated like propagation, on an `AtomicBool` that
  `otel::enable_metrics` sets (`install` calls it after `global::set_meter_provider`): without a
  provider a request pays one relaxed load, no clock read (bench: within noise). The future
  carries `Measured`, which is `()` without the feature. The meter provider becomes the global
  one so application instruments are exported too. `tests/metrics.rs` is its own binary: the
  provider is global.
- **Telemetry is configured by the environment, not by an API.** `App::serve_until` calls
  `otel::auto_init` (feature `otel`): with `OTEL_EXPORTER_OTLP_ENDPOINT` set and no subscriber
  installed (`tracing::dispatcher::has_been_set`), lesto installs console + OTLP (traces and
  logs) and flushes on
  shutdown, so `main` stays `app.serve().await`. Two refusals keep it honest: no export without
  an endpoint (the SDK default of `localhost:4318` would spam errors in every test run), and
  nothing at all once the application has its own subscriber — that is the escape hatch for
  gRPC, samplers or a logs pipeline. Only lesto's own knobs are parsed (`Config::read`, taking a
  getter so tests never write to the environment); the endpoint, headers and timeout are read by
  the exporter itself. Reading the incoming `traceparent` is gated on an `AtomicBool` that
  `install` sets next to the propagator (`otel::enable_propagation` for an application with its
  own subscriber): the API's default propagator is a no-op, and asking it per request buys a
  lookup and a header walk for an answer that cannot change.
- **OTLP over HTTP/protobuf with the *blocking* reqwest client.** The batch processor runs on a
  thread of its own with no tokio reactor, so an async client panics there with "there is no
  reactor running" — the blocking client is what that processor is built for. No TLS feature is
  enabled (`https` endpoints need your own exporter): `opentelemetry-http`'s rustls features do
  not resolve against reqwest 0.12, and pulling a TLS stack into every `otel` build to reach a
  collector that is usually a sidecar is the wrong trade. gRPC is not wired for the same reason
  (tonic, prost-grpc); `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` is reported as ignored, not silently.
- **Spans are `tracing`, not OpenTelemetry.** The request span and the store span carry the
  semantic-convention field names plus the `otel.name` / `otel.kind` / `otel.status_code` fields
  `tracing-opentelemetry` reads, so the spans themselves cost no dependency and work with any
  subscriber; the `otel` feature adds the export and the *incoming* context (`traceparent` →
  parent span), neither of which can be done without the OpenTelemetry crates. The span name lives in
  `otel.name` because `tracing` metadata is static and the name is `{method} {http.route}`.
  lesto installs no propagator: the API's default is a no-op and W3C, B3 and the rest live in
  different crates, so choosing one for the user would be wrong. `Trace` has no level knob —
  a `tracing` level must be a constant, and filtering is the subscriber's job.
- **Timeout and body limit are opt-in `Router::layer` calls of their own**, made before the
  shared stack: an application that does not set them pays nothing per request, and one that
  does pays one re-boxing. CORS, compression and request ids (`App::cors`, `compression`,
  `request_id`) are tower-http's layers, added *after* the shared stack, so problems and
  caught panics carry CORS headers and get compressed. The timeout is hand-written (not tower-http:
  its `TimeoutLayer` answers an empty body, and a problem is the whole point) and answers `503`
  — `408` means the *client* was slow, `504` means an upstream gateway was. The body limit is
  axum's own `DefaultBodyLimit`; lesto's `Json` turns its rejection into a `413` problem.
- **The trace layer is outermost but inside routing.** `Router::layer` runs after axum
  matched the request, which is what makes `MatchedPath` (`http.route`) available; being the
  outermost of the stack it still wraps the panic catcher and the problem rendering, so the span
  sees the status the client sees. `url.query` and the `X-Forwarded-*` headers are opt-in:
  the first is application data, the second is client-controlled unless a proxy rewrites it.
- **Store spans cover the transaction, not the statements, and are named after the store
  method.** One `CLIENT` span from `BEGIN` to commit/rollback, named `NoteStore::list` — read
  from `std::any::type_name` of the closure, which the compiler spells with the path of the
  function it was written in (`db::trace::operation_of`, falling back to `read`/`write`).
  `BEGIN DEFERRED` as a name told nobody what ran. Three fields on success (`otel.name`,
  `otel.kind`, `db.system.name`): no `db.operation.name`, which the conventions require only
  when readily available and lesto does not see the statements, and no `db.query.text`, since
  lesto's own statement carries the principal's identity through `SET LOCAL` and the queries
  belong to the closure (`RUST_LOG=sqlx::query=debug` gives those, inside the span). A failed
  requirement gets no span, because a 403 never reaches the database.
- **The OpenTelemetry layer is configured to say less**: `with_location(false)`,
  `with_threads(false)`, `with_tracked_inactivity(false)`. Source file, line, module, thread id,
  thread name and busy/idle timings are seven attributes per span repeating what the span name
  already says, on every span of every request.
- **`lesto::lambda` wraps `lambda_http`, it does not reimplement it.** The official runtime already
  turns API Gateway v1/v2, Function URL and ALB events into `http::Request`s and accepts any
  tower service, so the crate adds only what lesto users need: the Lambda-or-local switch on
  `AWS_LAMBDA_RUNTIME_API`, stage stripping by default, and an in-process test helper. The
  stage is stripped by a `tower::ServiceExt::map_request` around the router (it must run
  *before* routing; a `Router::layer` runs after) rather than through the runtime's
  `AWS_LAMBDA_HTTP_IGNORE_STAGE_IN_PATH` variable, which is process-wide. Docs pages link
  `openapi.json` relatively for the same reason (a stage or proxy prefix the app does not know).

## How lesto works (the parts that are not obvious)

- `routes![f]` expands to `f::__lesto_check(&set); set.add_described(<f as RouteInfo>::meta(),
  f, <f as RouteInfo>::describe)`. `describe` is generated by the route attribute: each argument
  goes through autoref specialization (`DescribeProbe`: `OperationInput::describe` when the impl
  exists, nothing otherwise), so an undocumented custom extractor compiles; `strict-docs` makes
  `check_strict_input::<T>` require `OperationInput` again. `RouteSet::add` (no macro) still
  documents from the handler's types and requires every argument to be documented. The
  `where` clauses of the generated `__lesto_check<S>` are the per-argument extractor checks, so
  the state `S` is inferred from the `App<S>` the set is added to (no `state = ..` needed). With
  a `State<T>` argument or `state = T` the checks run immediately, on the argument's span. A
  missing attribute yields `expected type, found function`.
- The route macro warns about `axum::Json<T>`/`axum::extract::Query<T>` with `T: Validate`
  through autoref specialization: `(&ValidationProbe::<Arg>::new()).__lesto_validation()` picks
  the `#[deprecated]` trait method when the impl for `ValidationProbe<axum::Json<T: Validate>>`
  applies, the silent one on `&ValidationProbe<T>` otherwise. A deprecation is the only warning
  a proc macro can raise on stable.
- A nested `/` route is documented at the prefix itself (that is where axum serves it).
- `App::merge` / `nest_router` / `From<Router<S>>` take a finished `axum::Router`: served, not
  documented (the handler types are gone by then), but inside every lesto layer.
- **Problems are rendered once.** `ProblemLayer` publishes the request URI and the
  the request URI in a `tokio::task_local!` (`error::RENDER`); `Problem::into_response` reads
  it, fills `instance`, serializes once and marks the response with the `ProblemRendered`
  extension. A `problem+json` response *without* that marker — built by hand, or built where
  the task-local is not visible, as in a spawned task — still takes the layer's slow path
  (buffer, parse, rewrite), which is also what keeps `instance` working there.
- `into_router` installs a `404` fallback (unless `App::fallback` was called), a `405`
  `method_not_allowed_fallback`, and then **one** `Router::layer` call with
  `tower_layer::Stack`: `RequestSpanLayer` outside `ProblemLayer` outside `CatchPanicLayer`.
  One call, because axum re-boxes every route and its future on each `.layer(..)`; that is also
  why `Trace::off()` no longer removes the span layer, it only takes a branch inside it.
- The panic catcher stays hand-rolled (`catch_unwind` around `call` and around `poll`) because
  `tower_http::catch_panic` returns `Response<UnsyncBoxBody<..>>`: it would re-box every
  response body, and axum would wrap it in `Body` again — an allocation per request to save a
  hundred lines.
- `App::serve` → `serve_at` → `serve_on` → `serve_until(listener, shutdown)`;
  `shutdown_signal()` is `SIGTERM` or `Ctrl-C`. Graceful shutdown is `axum::serve(..)
  .with_graceful_shutdown`, raced against `shutdown_timeout` (30 s default) once the signal
  fires: a hung handler must not outlive the orchestrator's grace period. Connections still open
  then are dropped with the runtime when `main` returns (axum spawns them; there is nothing to
  abort from here).
- `status = N` wraps the handler (`WithStatus`) and rewrites a `200` into `N` at runtime, in a
  pin-projected future around the handler's own — no boxing.
- The docs routes (`/openapi.json`, `/docs`, `/swagger`) are built once into `Bytes` and carry
  an `ETag` hashed once, so a response is a reference-count increment and a conditional request
  is a `304`.
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
- schemars runs with a transform replacing boolean schemas (Swagger UI cannot render `true`).
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
- **The principal is authenticated once per request.** `Store::from_request_parts` caches it in
  the request extensions as an `Arc<P>`, so a handler with two stores verifies one token;
  `into_principal` therefore hands out the `Arc`. `Arc` rather than a `Clone` bound on
  `Authenticated`, which every user's principal would have to carry. Failures are not cached.
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
| `lesto cannot document X as a handler argument` | (only with feature `strict-docs` or `RouteSet::add`) derive `JsonSchema` on the payload and use `Json/Query/Path`, or `impl OperationInput for X {}` |
| `lesto cannot document X as a handler response` | return `Json<T>` (T: Serialize + JsonSchema), or `impl OperationOutput for X {}` |
| `X cannot be a handler argument in this position` | body extractor must be the last argument |
| `X is not an extractor` | payload type lacks `Deserialize`/`JsonSchema`/`Validate`, or the extractor expects a state other than the `App<S>` it is registered on (`state = AppState` pins it) |
| `expected type, found function f` in `routes![f]` | the handler is missing `#[lesto::get(...)]` |
| `cannot find garde/schemars in the crate root` | use `#[lesto::model]` instead of the plain derives, or add the crates as direct dependencies |
| `type annotations needed for App<_>` | write `App::<()>::new()` / `App::<AppState>::new()` |
| `this store is read-only: ReadOnly does not allow write` | declare `YourStore<ReadWrite, _>` or bound the impl with `M: Writable` |
| `Public cannot hold permissions` | pass `Anyone`, or use an `Authenticated` principal in the handler |
| `X is not a principal for state S` | `impl Authenticated for X` with `type State = S`, or use `Public` |
| `#[derive(Store)] expects a tuple struct` | `struct S<M, P>(lesto::db::Store<M, P, Db>);` |
| warning: `axum::Json<T>` ... does not run `T`'s garde rules | use `lesto::Json`/`lesto::Query`, or `#[allow(deprecated)]` on the handler |
| `type annotations needed` on `.into()` in a store closure | use `Error::not_found(..)`/`Error::conflict(..)`/`Error::http(e)` |

## Roadmap and non-goals

The ordered, executable queue lives in [`docs/review/plan-production-ready.md`](docs/review/plan-production-ready.md):
what blocks publication (key order as a documented guarantee, the panicking `IntoStatus`, the
public API audit and the breaking decisions), then production robustness (shutdown deadline,
timeout, body limit), then "batteries included" (`#[lesto::main]`, `#[lesto::model]`, one line
in every manifest), then axum interoperability and the rest of the tower-http batteries. [`plan-oidc.md`](docs/review/plan-oidc.md) is the queue for roadmap item 2 (OAuth2 / JWT,
branch `oidc`). [`plan-B-performance.md`](docs/review/plan-B-performance.md)
is the finished performance workstream, with its numbers.

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
(scaffold); `AnyOf`/`AllOf` requirements; documenting per-operation
permissions as OpenAPI scopes; `--watch`/`--ignore` for `lesto dev`; exporting the OpenAPI document as 3.0 with
`x-amazon-apigateway-integration` extensions so an API Gateway can be created from it (API
Gateway imports 3.0 only).

Out of scope unless asked: actix-web backend, websockets, multipart, typed header/cookie
parameters beyond API keys, an ORM or query builder, role hierarchies or permission wildcards
(`has_permission` is user code), `sqlx::Any`, hot patching without restart.

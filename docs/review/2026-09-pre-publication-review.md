# lesto: pre-publication review

*First version: 2026-09-18. Revised the same day after the maintainer stated the project goals
(section 1). Scope: `crates/lesto` (core, `db`, `trace`, `security`), `crates/lesto-macros`,
`examples/02-notes`. Purpose: record how a senior Rust developer is likely to react, decide which
criticisms to act on, and provide the input for the action plan (section 10).*

---

## 1. Project goals (decided)

These goals are fixed. The review judges lesto against them. Criticisms that contradict them are
listed in section 8 with the answer lesto gives, not as problems to fix.

1. **Batteries included.** One dependency gives a complete experience. lesto composes existing
   libraries in an opinionated way, as FastAPI composes Starlette and pydantic. The Rust ecosystem
   already has many minimal, pluggable frameworks; lesto deliberately goes the other way.
   Databases, authentication, observability, the Lambda adapter, and the dev CLI are part of the
   product, not scope creep.
2. **Configure and go.** Telemetry and serving are configured with environment variables; `main`
   stays one line.
3. **Very fast.** Framework overhead over plain axum must be within measurement noise on the
   happy path.
4. **Easy to adopt from axum, easy to leave for axum.** An axum user can bring existing routers,
   extractors, and middleware. A lesto user can drop back to a plain `axum::Router` without
   rewriting handlers.
5. **Standard middleware.** Every middleware is a tower `Layer`, usable with or without `App`.

---

## 2. Verdict

- **The handler layer feels native.** It is axum with Rocket-style attributes: `#[lesto::get]`,
  `routes![]`, extractors, tower layers, and `into_router()` / `map_router`. A senior recognizes
  the shape in seconds.
- **Engineering quality is above average for a 0.1:** compiler diagnostics written by the
  framework (`#[diagnostic::on_unimplemented]` + UI tests), secrets redacted in `Debug`, RFC 9457
  everywhere, no `set_var`, graceful shutdown, `LISTEN_FDS` socket handover, and written design
  decisions.
- **Gaps against the goals:**
  1. *Batteries included* is not yet true for dependencies: users must add `serde`, `garde`,
     `schemars`, and `tokio` themselves (section 5).
  2. *Very fast* is not yet true: 0.5–2 µs of overhead per request, and 6x axum on the 404 path
     (section 3).
  3. *Standard middleware* is not yet true: lesto's own middleware is `axum::middleware::from_fn`
     closures, which cannot be reused on a plain router (section 6).
  4. *Easy to adopt* has friction: no way to mount an existing `axum::Router` into an `App`, and a
     custom axum extractor needs an empty `impl OperationInput` before it compiles (section 6).
- **One correctness leak** that is independent of scope: lesto turns on `serde_json/preserve_order`
  for the user's whole dependency graph (section 4.1).

---

## 3. Performance

### 3.1 Measurement

In-process benchmark: `tower::Service::call` in a loop on a `current_thread` runtime, release
build, 300,000 iterations after a 20,000-iteration warm-up, no `tracing` subscriber installed. This
isolates framework overhead from networking. Two runs gave consistent results. Source in the
appendix.

| Scenario | axum 0.8 | lesto (default) | lesto `Trace::off()` |
|---|---:|---:|---:|
| `GET /hello` (static `&str`) | ~620 ns | ~1,510 ns | ~1,140 ns |
| `POST /items` (JSON + garde validation, `status = 201`) | ~1,200 ns | ~2,200 ns | ~1,820 ns |
| `GET /missing` (404) | ~380 ns | ~2,300 ns | ~1,850 ns |

The absolute overhead is small compared with networking and databases. It still contradicts
goal 3, and "2.5–6x slower than axum" is the headline a benchmark-minded reviewer will post.

> **Update, 2026-09-20 (workstream B done, items B0–B9).** The benchmark now lives in the
> repository (`crates/lesto/benches/overhead.rs`, `cargo bench -p lesto`) and the table above
> reads:
>
> | Scenario | axum 0.8 | lesto (default) | lesto `Trace::off()` |
> |---|---:|---:|---:|
> | `GET /hello` | 616 ns | 911 ns | 864 ns |
> | `POST /items` (JSON + garde validation, `status = 201`) | 1,187 ns | 1,498 ns | 1,477 ns |
> | `GET /missing` (404) | 388 ns | 1,260 ns | 1,230 ns |
>
> The 404 path is now 872 ns over axum, under the 1 µs the plan asked for; the happy path is
> 295 ns over, which is the request span and the two remaining services. Details, the rest of
> the cases and the B10 spike are in [the workstream B task list](plan-B-performance.md).

### 3.2 Causes

1. **`finish_problem` middleware** (`crates/lesto/src/app.rs:608`).
   - It is an `axum::middleware::from_fn` on every request, which means boxing and a `Next`.
   - It allocates a `String` for the request path on every request, error or not.
   - On every `application/problem+json` response it runs `to_bytes`, then
     `serde_json::from_slice::<Problem>`, then `serde_json::to_vec`. That is serialize, parse, and
     serialize again, and it explains the 404 cost.
2. **Trace middleware** (`crates/lesto/src/trace.rs`).
   - The module docs claim that with no subscriber the span "costs one atomic load". Measured:
     about 370 ns (default vs `Trace::off()`), almost all of it the `from_fn` wrapper.
   - A false performance claim costs more credibility than the overhead itself.
3. **`CatchPanic`** (`app.rs:663`): two `Box::pin` per request.
4. **`WithStatus`** (`route.rs:1297`): one more boxed future per request on every route whose
   `status` is not 200.
5. **Docs routes** (`app.rs:373`, `:393`, `:407`): `json.as_str().to_owned()` copies the full
   OpenAPI document (and the HTML pages) on every request.
6. **OpenTelemetry propagation** (`trace.rs:395`, feature `otel`): `set_parent` queries the global
   propagator on every request, even when no exporter was installed.

### 3.3 Fixes

1. Render each problem once. *Revised in plan B (tasks B1–B2):* a `ProblemLayer` puts the
   request path and the `ErrorFormat` in a request-scoped context (`tokio::task_local!`), and
   `Problem::into_response` reads it to fill `instance` and pick the format when it serializes.
   Outside the layer (a plain `Router`) problems still render, without `instance`. This is
   preferred over carrying the `Problem` in response extensions with an empty body, which would
   leave responses built outside the layer without a body.
2. All of lesto's middleware become hand-written tower `Layer`/`Service` types with concrete,
   pin-projected futures (`pin-project-lite`), no boxing. This also serves goal 5.
3. Rewrite `CatchPanic` with a pin-projected future and no boxing. *Revised:*
   `tower_http::catch_panic` 0.6 re-boxes every response body (`UnsyncBoxBody`), so it would add
   an allocation per request. Keep lesto's own catcher and confirm the choice with a benchmark
   (see [plan B, task B4](plan-B-performance.md)).
4. `WithStatus` gets a concrete future instead of a boxed one.
5. Docs routes serve a `Bytes` clone (a reference-count increment).
6. Propagation runs only when lesto installed OpenTelemetry (a flag decided once at startup).
7. Correct the "one atomic load" sentence, or make it true.
8. Keep a benchmark in the repository (criterion for per-request overhead, `oha` for throughput)
   and a regression check in CI.

**Target:** overhead within noise of axum on the happy path; under 1 µs on the 404 path.

### 3.4 Database layer

- **Three round trips for one `SELECT`.** Every `Store::read` sends `BEGIN [READ ONLY]`, the
  query, and `COMMIT`; plain `sqlx` needs one. Option: skip the transaction for `read` when there
  are no `TransactionSettings` and the isolation is the default. Open question: the read-only
  guarantee then relies on the connection or replica, not on the transaction (section 9).
- **Repeated authentication.** The principal is authenticated once *per store argument*; nothing is
  cached in request `extensions`. A handler with two stores verifies the token twice. Fix: cache
  the principal in `Parts::extensions`.

---

## 4. Idiomatic Rust: criticisms to act on

### 4.1 Feature unification leaks into the user's build (verified)

`cargo tree -e features` on a crate that depends on `lesto` shows:

- **`serde_json/preserve_order`** (from the workspace dependency and from
  `schemars/preserve_order`). Feature unification turns `serde_json::Map` into an `IndexMap` for
  *every* crate in the user's graph. This changes key order and performance in code that has
  nothing to do with lesto. This is a correctness issue, not a scope issue.
- **`tokio/full`**: lesto should request the tokio features it uses. Once `#[lesto::main]` exists
  (section 5), lesto owns the runtime, and the feature set becomes lesto's decision.
- **`garde/email`, `garde/url`**: pull regex and idna into every build. Expose them as lesto
  features. They can be on by default, since lesto is batteries included, but the user should be
  able to turn them off.

**Fix:** keep key order inside lesto without the global feature (sort or rebuild the maps the
OpenAPI output needs), and set the tokio and garde feature lists deliberately.

### 4.2 Status override by rewriting at runtime

- `status = 201` wraps the handler and rewrites *any* `200` into `201` (`route.rs:1314`). A route
  declared 201 can never answer 200 on purpose.
- It also contradicts "documentation from types".
- **Proposal:** response wrappers `Created<T>`, `Accepted<T>`, `NoContent`, which implement
  `IntoResponse + OperationOutput`. Keep `status = N` as a shortcut if desired.

### 4.3 Two `Json` types: validation can be lost without warning

- `lesto::Json` shadows `axum::Json`, and lesto implements `OperationInput` for `axum::Json`,
  `axum::extract::Query`, and `axum::extract::Path` (`operation.rs:323–341`).
- A handler that uses axum's `Json` gets full documentation and **no validation**, with no
  warning.
- **Tension with goal 4.** An axum user who brings existing handlers expects `axum::Json` to keep
  working. Removing the impls breaks adoption; keeping them silently hides a missing validation.
- **Proposal:** keep the impls, and make the route macro emit a warning (via a deprecated
  marker item, the usual trick for warnings from proc macros) when the argument type is
  `axum::Json<T>` / `axum::extract::Query<T>` with `T: Validate`. The warning names `lesto::Json`.

### 4.4 `IntoStatus for u16` panics at runtime

- `error.rs:41`: `StatusCode::from_u16(self).expect("invalid HTTP status code")`.
- `HttpError::new(upstream_status, ...)` with a dynamic value panics inside the handler. The panic
  catcher answers 500, but this contradicts the "no panics on user input" convention.
- **Fix:** map an invalid code to 500 and log it, or accept only `StatusCode` and `TryFrom<u16>`.

### 4.5 `lesto::db` design points

The layer stays (goal 1). These points are about its design, not its existence.

- **Implicit 404.** `sqlx::Error::RowNotFound` always becomes 404. A `fetch_one` on a *secondary*
  lookup (a foreign key, a configuration row) then answers "Not found" instead of a 500, which
  hides bugs. Options: an explicit `.or_not_found()`, or keep the mapping and document the risk
  prominently.
- **Unique and foreign key violations become 409 automatically.** Same concern, lower risk.
- **Permissions are strings** (`&'static str`). Show in the tutorial how to implement
  `Requirement<P>` for a user-defined enum, so strings are one option and not the only one.
- **`Authenticated::State` is an associated type.** A principal works with one state type. The
  coherence reason is recorded in AGENTS.md; document the limitation for users as well.
- **Transaction per method, no composition.** Documented design decision. Expect "how do I call two
  store methods atomically?" early; the answer (`..._in(conn, ..)` helpers) should be in the
  tutorial's FAQ.

### 4.6 Semver coupling

- **axum.** The public API exposes axum types (`Router`, `Handler`, re-exported `axum`). axum 0.9
  forces a lesto major release. State the policy in the README ("lesto X.Y tracks axum 0.8").
- **sqlx.** See section 7.1.

### 4.7 Minor points

- **Hand-written OpenAPI model** (`openapi.rs`) instead of `utoipa::openapi`, `oas3`, or
  `openapiv3`. Small and under control; a reviewer may call it reinvention.
- **Marker struct with the handler's name** (`#[allow(non_camel_case_types)] struct create_user {}`).
  Rocket does the same, so it is accepted.
- **`HttpError` mixes public fields** (`status`, `detail`) with private boxed extras. Pick one
  style.
- **MSRV 1.94** is aggressive; it is dictated by sqlx 0.9. Say so.
- **Compile times** were not measured. Measure a clean and an incremental build of
  `examples/02-notes` and publish the numbers, especially once `otel` is on by default
  (section 7.2).
- **Docs assets load from jsDelivr by default** (with SRI hashes). Mention
  `scalar_script_url` / `swagger_ui_base_url` near the top of the docs for offline users.

---

## 5. Batteries included: dependencies (decided)

Today a lesto application's `Cargo.toml` needs `lesto`, `serde`, `garde`, `schemars`, and `tokio`,
because the derive macros emit absolute paths (`::serde`, `::garde`, `::schemars`) and `main`
needs `#[tokio::main]`. This contradicts goal 1 more than any external criticism does.

**Decision:** add `#[lesto::model]` and `#[lesto::main]`. Target `Cargo.toml`:

```toml
[dependencies]
lesto = "0.1"   # features = ["sqlite"] etc. as needed
```

### 5.1 `#[lesto::main]`

- Expands to `#[::lesto::tokio::main(crate = "::lesto::tokio")]`. `tokio::main` supports the
  `crate` option (verified in `tokio-macros` 2.7).
- lesto re-exports `tokio` and chooses its feature set (section 4.1).
- Room for later: runtime flavor and worker count from environment variables, consistent with
  goal 2.

### 5.2 `#[lesto::model]`

- Adds the derives a model needs and points each at lesto's re-exports:
  - serde: `#[serde(crate = "::lesto::serde")]` (supported by serde);
  - schemars: `#[schemars(crate = "::lesto::schemars")]` (supported by schemars 1.x);
  - garde: **not supported.** `garde_derive` 0.23 hard-codes `::garde::...` paths (verified in
    `garde_derive/src/emit.rs`). Options, in order of preference:
    1. contribute a `#[garde(crate = "...")]` option upstream (small, self-contained change);
    2. until it lands, document `garde` as the one extra dependency;
    3. last resort: vendor a patched `garde_derive` into `lesto-macros` (maintenance cost).
- Relationship with `#[lesto::views]`: `model` should compose with `views` (both attributes on one
  struct, or `#[lesto::model(views(Create(..), Update(..?)))]`). Decide in the design.
- **Database models.** `sqlx::FromRow` needs `sqlx` as a direct dependency. Check whether the
  sqlx derive accepts a crate path. If it does not, the same three options apply, and users of the
  `db` feature also need `sqlx` for queries unless lesto re-exports it as `lesto::db::sqlx`.

---

## 6. axum interoperability and middleware (decided direction)

### 6.1 Adopting lesto from axum

| Need | Today | Proposal |
|---|---|---|
| Mount an existing `axum::Router` | only `map_router(|r| r.merge(..))` | `App::merge(router)` and `App::nest_router(prefix, router)`, undocumented routes. Optionally `App::from(Router)`. |
| Use an existing custom extractor | compile error until the user writes `impl OperationInput for X {}` | Accept undocumented extractors: the route macro uses autoref specialization to call `describe` when an `OperationInput` impl exists and do nothing otherwise, optionally with a warning. |
| Use `axum::Json` / `Query` | documented, not validated, no warning | warning, see section 4.3 |
| Existing tower middleware | `App::layer` accepts any tower `Layer` | already fine |
| Existing `State` / `FromRef` setup | works | already fine |

### 6.2 Leaving lesto for axum

- `into_router()` already returns a plain `axum::Router`.
- Once lesto's middleware is public tower layers (section 6.3), a user who leaves keeps
  `lesto::Json`, `lesto::Query`, `HttpError`, and `ProblemLayer` on a plain `Router`. They leave
  `App`, not every piece.
- Handlers already stay plain functions; the route attribute does not change them.
- **Tutorial:** add a "Leaving lesto" chapter showing the exit step by step. This is also the best
  argument for adoption: nobody is locked in.

### 6.3 Standard middleware: tower (and tower-http)

- lesto's own middleware becomes public tower types, usable on any `Router`: `ProblemLayer`
  (instance + error format), `TraceLayer` (lesto's semantic-convention span; the name may clash
  with tower-http's, pick a distinct one such as `RequestSpanLayer`), and a catch-panic layer.
- **Include `tower-http`**, which the previous version of AGENTS.md avoided on purpose. Under goal 1
  it is part of the batteries: CORS, compression, timeouts, request body limits, request IDs,
  and sensitive-header marking. These are the equivalents of FastAPI's
  `CORSMiddleware`, `GZipMiddleware`, and `TrustedHostMiddleware`.
- Configuration: builder methods on `App` (`.cors(..)`, `.compression()`, `.timeout(..)`,
  `.body_limit(..)`), each installing the tower-http layer. The same layers stay usable directly
  with `App::layer` or on a plain `Router`.
- Update AGENTS.md: the panic catcher stays hand-rolled, but the reason changes from "avoid a tower-http dependency" to "tower-http re-boxes every response body" (plan B, task B4).

---

## 7. Feature decisions

### 7.1 `db`: keep as a feature of `lesto` (decided)

The maintainer's criterion: move `db` into a separate crate only for a real advantage, such as
performance when it is not used.

- **Runtime cost when unused: none.** The code is behind `#[cfg(feature = "db")]`, so it does not
  exist in the binary.
- **Compile cost when unused: none.** Neither lesto's `db` modules nor sqlx are compiled.
- **The only non-marketing advantage of a separate crate is semver isolation.** sqlx types are in
  lesto's public API (`Connection<DB>`, `Error::Sqlx(sqlx::Error)`, `Db<DB: sqlx::Database>`).
  A breaking sqlx release (0.10) forces a major release of `lesto` for every user, including users
  without `db`. Before 1.0 this does not matter, because every minor release may already break.
- **Decision:** keep `db` as a feature. Revisit at 1.0, and only for the semver reason.

### 7.2 `otel`: part of the experience; make "set variables and go" real

- Today `default = []`, so the user must enable the `otel` feature before the variables do
  anything. That is one step more than goal 2 allows.
- **Proposal:** `default = ["otel"]`. Before deciding, measure:
  - the compile-time cost of `opentelemetry`, the SDK, the OTLP exporter, and blocking `reqwest`
    on a clean build of `examples/01-hello`;
  - the per-request runtime cost with the feature on and no endpoint set, after fix 6 in
    section 3.3 (target: zero).
- **Global subscriber.** Installing a global `tracing` subscriber from a library breaks a common
  Rust rule. lesto's answer: it is an application framework that owns `main`, as uvicorn owns
  logging setup for FastAPI. The escape hatch already exists: install your own subscriber first,
  and lesto stands aside. Put this answer in the README, not only in chapter 15.

---

## 8. Criticisms answered by the goals (no change)

These will be raised. The answer is recorded here so it is given consistently.

| Criticism | Answer |
|---|---|
| "Too much for one framework / kitchen sink" | Batteries included is the goal (1). The Rust ecosystem already has minimal frameworks; lesto is the opposite choice, as FastAPI is in Python. Every part is a feature flag: what is not used is not compiled. |
| "It is just FastAPI for Rust" | Yes, on purpose: the same approach (compose proven libraries into one opinionated experience), built from Rust's equivalents: axum, serde, garde, schemars, sqlx, tracing, OpenTelemetry. |
| "A library must not install a global subscriber" | lesto owns `main` as an application framework; it installs nothing when a subscriber already exists. See 7.2. |
| "Configuration through environment variables is magic" | It is goal 2, and it follows twelve-factor conventions and the OpenTelemetry specification (`OTEL_*`). Every setting also has an explicit API (`serve_at`, `serve_on`, `otel::init`). |
| "Why not aide / utoipa + garde yourself?" | Assembling them is exactly the work lesto removes. See section 11 for the comparison. |
| "The database layer belongs in its own crate" | It costs nothing when unused (7.1). The only real reason is semver, relevant after 1.0. |

---

## 9. Open questions for the action plan

1. **garde crate path:** upstream PR, document the extra dependency, or vendor (section 5.2)?
2. **sqlx derive crate path** for `FromRow`, and whether to re-export `sqlx` as `lesto::db::sqlx`.
3. **`#[lesto::model]` and `#[lesto::views]`:** two attributes, or one with a `views(..)` option?
4. **`otel` in the default features:** decide after the compile-time and runtime measurements (7.2).
5. **Status:** `Created<T>`-style wrappers alongside `status = N`, or instead of it (4.2)?
6. **Undocumented extractors:** silent, warning, or error behind a `strict_docs()` mode (6.1)?
7. **`db` reads without a transaction** when there are no settings and default isolation: worth
   the loss of `BEGIN READ ONLY` enforcement (3.4)?
8. **Implicit `RowNotFound` → 404:** keep and document, or make explicit (4.5)?
9. **Name of lesto's span layer**, so it does not clash with `tower_http::trace::TraceLayer`.

---

## 10. Input for the action plan

Grouped by workstream, in suggested order. Each item references the section with the details.

### A. Correctness and blockers (before any publication)
1. Remove the `serde_json/preserve_order` leak; set tokio and garde features deliberately (4.1).
2. Make `IntoStatus` non-panicking (4.4).
3. Fix or remove the "one atomic load" claim (3.2).

### B. Performance (goal 3)
4. `Problem` in response extensions + `ProblemLayer`, no JSON re-parse (3.3.1).
5. Hand-written tower layers with concrete futures for trace and problem handling (3.3.2).
6. Catch-panic layer without boxing (3.3.3; tower-http's re-boxes bodies).
7. Unboxed `WithStatus`; `Bytes` for the docs routes; propagation only when OTel is active
   (3.3.4–6).
8. Benchmarks in the repository and a regression check in CI (3.3.8).
9. `db`: cache the principal per request; decide on reads without a transaction (3.4).

### C. Batteries included: dependencies (goal 1)
10. `#[lesto::main]` with a re-exported tokio (5.1).
11. `#[lesto::model]` with serde and schemars crate paths; garde path resolved (5.2).
12. Re-export sqlx for `db` users if the derive path allows it (5.2).
13. Update the tutorial, README, and `examples/*` so `Cargo.toml` shows only `lesto`.

### D. axum interoperability and middleware (goals 4 and 5)
14. Public tower layers for lesto's middleware (6.3).
15. tower-http as a dependency; `App` builder methods for CORS, compression, timeout, body limit,
    request ID (6.3).
16. `App::merge` / `App::nest_router` for existing axum routers (6.1).
17. Accept undocumented custom extractors (6.1).
18. Warning for `axum::Json` / `Query` with a `Validate` payload (4.3).
19. Tutorial chapter "Leaving lesto" (6.2).

### E. API polish
20. `Created<T>` / `Accepted<T>` / `NoContent` response types (4.2).
21. `db` design points: explicit or documented `RowNotFound`, enum permissions example, FAQ on
    composing store methods (4.5).
22. `HttpError` field style (4.7).

### F. Positioning and documentation
23. Decide `otel` in default features after measuring (7.2).
24. README: goals first ("batteries included, like FastAPI"); comparison with the alternatives
    (section 11); axum version policy; OTel auto-init and the subscriber escape hatch on the first
    page.
25. Publish benchmark and compile-time numbers with the method.
26. Update AGENTS.md: goals, tower-http decision, `model`/`main`, layer names.

---

## 11. The Rust web framework landscape

Maturity and activity are qualitative assessments as of mid-2026. Verify current releases and
download numbers before quoting them publicly.

### 11.1 Comparison table

| Framework | Built on | Routing style | OpenAPI | Request validation | Error format | Batteries (DB, auth, CLI, observability) | Raw performance | Maturity / activity |
|---|---|---|---|---|---|---|---|---|
| **lesto** | axum 0.8 | Attribute macros + `routes![]` | **Built in, from handler types, no annotations**; Scalar + Swagger UI | **Built in** (garde, in `Json`/`Query`) | **RFC 9457 by default**, FastAPI shape optional | sqlx stores with permissions and RLS, Bearer/Basic/API key, OTel traces + logs, Lambda, `lesto dev` | axum + 0.5–2 µs today (plan in section 3) | Unpublished, one maintainer |
| **axum** | hyper + tower (tokio-rs) | Builder (`Router::route`) | None; add-ons: utoipa, aide | None; add-ons: garde, validator, `axum-valid` | Ad hoc | None; large ecosystem (tower-http, axum-extra) | Top tier | De facto standard, very active |
| **axum + aide** | axum | Builder (`api_route`) | **From handler types** (`OperationInput`/`OperationOutput`), some annotations | Third party | Ad hoc | None | Same as axum | Active, smaller community, pre-1.0 |
| **axum + utoipa** | axum (also actix, Rocket) | Builder + `#[utoipa::path(...)]` | **Annotation-driven** (paths and responses repeated in the macro) | Third party | Ad hoc | None | Same as axum | Most popular OpenAPI crate, active |
| **actix-web** | actix-rt (tokio) | Attribute macros or builder | None built in; utoipa, apistos, paperclip | Third party | Ad hoc | Large ecosystem | Top tier | Mature, stable 4.x, active |
| **Rocket** | hyper (tokio) | Attribute macros + `routes![]` | None built in; `rocket_okapi` | Built-in forms and guards | Catchers | Config, templates, `rocket_db_pools` | Good | 0.5 stable since 2023, slow release cadence |
| **poem + poem-openapi** | hyper (tokio) | `#[OpenApi] impl` blocks with `#[oai]` methods | **Built in, from types**; Swagger UI, RapiDoc, Redoc | **Built in** (`#[oai(validator(...))]`) | Typed response enums | Moderate | Good | Active, smaller English-speaking community |
| **salvo** | hyper (tokio) | Tree router + `#[handler]` / `#[endpoint]` | **Built in** (`oapi` feature) | Partial / third party | Custom | Many: ACME, HTTP/3, proxy, caching | Good | Active, community mostly Chinese-speaking |
| **dropshot** (Oxide) | hyper (tokio) | `#[endpoint]` + API traits | **Built in, OpenAPI-first**, spec checked into the repo | Via schemars/serde types | Own `HttpError` shape | None; deliberately small, JSON only | Good | Production at Oxide, active, opinionated |
| **loco.rs** | axum | Rails-style controllers + generators | Via utoipa integration | Third party | Framework-defined | **Full stack**: SeaORM, migrations, auth, workers, mailers, generators | Same as axum | Active, growing |
| **Pavex** | hyper (tokio), code generation | Blueprint + compile-time DI | Not the focus | Framework extractors | Framework-defined | DI, configuration | Good (no runtime DI cost) | Pre-1.0, one main author |
| **cot** | axum/tower | Django-style apps and views | Early | Forms | Framework-defined | **Full stack**: ORM, admin, auth, templates | Good | Very early (2025+), active |
| **warp** | hyper (tokio) | Filter combinators | None | Third party | Rejections | None | Good | Maintenance mode |
| **ntex** | own runtime layer (tokio / compio) | Actix-like | None | Third party | Ad hoc | Moderate | Top tier | Active, niche |
| **tide / gotham** | async-std / hyper | Builder | None | None | Ad hoc | None | — | Effectively unmaintained |

### 11.2 Where lesto sits, given its goals

- **Minimal and pluggable** (the majority): axum, actix-web, warp, ntex, Pavex, and axum with aide
  or utoipa. These are what lesto is deliberately *not*.
- **Batteries included:**
  - **loco.rs**: on axum, but aimed at server-rendered, Rails-style full-stack apps (ORM,
    generators, mailers). OpenAPI is an add-on.
  - **cot**: Django-style (admin, ORM, templates), very early.
  - **Rocket**: some batteries, no OpenAPI story, slow cadence.
- **API-first with OpenAPI built in:** poem-openapi, dropshot, salvo. None of them is built on
  axum, and none ships a data layer or observability.

**lesto is the only option that is both batteries included and API-first on axum.** No project
in the table combines:

1. axum compatibility, with a way back to a plain `Router`;
2. OpenAPI from the handler signature and doc comments, no annotations;
3. validation inside the extractors;
4. RFC 9457 errors by default, including 404, 405, and panics;
5. an integrated data layer with permissions and row level security;
6. OpenTelemetry traces and logs configured by environment variables;
7. a FastAPI-like dev loop (`lesto dev`, docs at `/docs`).

The closest alternatives each cover part of it:
- **loco.rs** is batteries included on axum, but aimed at full-stack apps, not APIs;
- **poem-openapi** and **dropshot** are API-first, but not on axum and without batteries;
- **axum + aide + garde** gives 2 and 3 if you assemble it yourself.

### 11.3 Decision

**Continue with lesto.** The gap it targets (FastAPI's approach, applied to Rust's best libraries,
on axum) is not covered by any existing project. The work needed is in section 10. The
performance work is days, not months. The dependency and interoperability work (`model`, `main`,
tower layers, `merge`) is what makes the batteries-included and "easy in, easy out" promises
true.

---

## 12. What a senior will like

- **Escape hatches:** `into_router()`, `map_router`, `layer`, plain `State` / `Extension`. With the
  public tower layers (6.3), the exit becomes complete.
- **Diagnostics as a feature:** messages that say what to do, tested with UI tests that pin only
  lesto's own text.
- **Handlers stay plain functions** that can be called in tests.
- **Production defaults:** RFC 9457 everywhere, redacted credentials, no leaked internals,
  graceful shutdown on `SIGTERM`, systemd-compatible socket activation.
- **Modern Rust used well:** `AsyncFnOnce` / `AsyncFn` closures in stores with `Send` futures and
  no boxing, `let` chains, edition 2024, no `set_var`.
- **Written design decisions** in AGENTS.md.

---

## Appendix: benchmark source

The benchmark used for section 3.1, run with `cargo run --release`:

```rust
use std::time::Instant;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use lesto::prelude::*;
use tower::{Service, ServiceExt};

#[derive(Deserialize, Serialize, JsonSchema, Validate)]
struct Item { #[garde(length(min = 1))] name: String, #[garde(range(min = 0))] qty: i64 }

#[lesto::get("/hello")]
async fn hello() -> &'static str { "hi" }
#[lesto::post("/items", status = 201)]
async fn create(Json(i): Json<Item>) -> Json<Item> { Json(i) }

async fn ax_hello() -> &'static str { "hi" }
async fn ax_create(axum::Json(i): axum::Json<Item>) -> (axum::http::StatusCode, axum::Json<Item>) {
    (axum::http::StatusCode::CREATED, axum::Json(i))
}

fn req(kind: u8) -> Request<Body> {
    match kind {
        0 => Request::get("/hello").body(Body::empty()).unwrap(),
        1 => Request::post("/items")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"name":"abc","qty":3}"#))
            .unwrap(),
        _ => Request::get("/missing").body(Body::empty()).unwrap(),
    }
}

async fn run(name: &str, mut svc: axum::Router, kind: u8) {
    let n = 300_000;
    for _ in 0..20_000 {
        let r = ServiceExt::<Request<Body>>::ready(&mut svc).await.unwrap().call(req(kind)).await.unwrap();
        let _ = r.into_body().collect().await;
    }
    let t = Instant::now();
    for _ in 0..n {
        let r = ServiceExt::<Request<Body>>::ready(&mut svc).await.unwrap().call(req(kind)).await.unwrap();
        let _ = r.into_body().collect().await;
    }
    println!("{name:40} {:8.0} ns/req", t.elapsed().as_nanos() as f64 / n as f64);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let ax = || axum::Router::new()
        .route("/hello", axum::routing::get(ax_hello))
        .route("/items", axum::routing::post(ax_create));
    let le = || App::new().routes(routes![hello, create]).into_router();
    let le_off = || App::new().trace(Trace::off()).routes(routes![hello, create]).into_router();
    for (k, label) in [(0u8, "GET /hello"), (1, "POST /items json+validate"), (2, "GET /missing 404")] {
        run(&format!("axum    {label}"), ax(), k).await;
        run(&format!("lesto   {label}"), le(), k).await;
        run(&format!("lesto trace off {label}"), le_off(), k).await;
    }
}
```

Dependencies: `lesto` (path), `axum 0.8`, `tokio` (`full`), `tower 0.5` (`util`), `serde`,
`garde 0.23` (`derive`), `schemars 1` (`derive`), `http-body-util 0.1`. The results measure
per-request framework overhead only. They do not include networking and are not a throughput
benchmark.

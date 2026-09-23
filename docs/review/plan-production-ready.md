# Road to production ready: the task list

*Written 2026-09-20, after workstream B (performance) landed; revised 2026-09-23 (key order
decided, API freeze and robustness blocks added). It replaces section 10 of
[the pre-publication review](2026-09-pre-publication-review.md) as the working list: the review
stays the reasoning, this file is the queue.*

Tasks are ordered by priority. Everything in **P0** blocks publication on crates.io; **P1** is
what a service needs before it runs in production; **P2** blocks the claim the project is built
on. Lower priorities are real work, not filler, but shipping without them is a decision rather
than an accident.

## How to use this file in a fresh session

1. Read `AGENTS.md` first — conventions, design decisions, and the commands below come from it.
2. Pick the **first unchecked task**, unless the priority order is overridden by the maintainer.
   Tasks inside one priority block are independent unless "Depends on" says otherwise.
3. Do the whole task, including its documentation line: a behavior change updates the tutorial
   chapter and `README.md` in the same commit.
4. Run the gate before claiming it is done:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo check -p lesto --no-default-features           # and --features db, --features otel
cargo bench -p lesto                                 # when the task touches a request path
sh docs/build.sh                                     # when docs changed
git grep -i presto                                   # the old name must not come back
```

5. Tick the box here, with the numbers or the decision taken, and commit (one logical change per
   commit, imperative subject, body explains why).

## Where the project stands against its five goals

| Goal | State | What is missing |
|---|---|---|
| 1. Batteries included | met (2026-09-23) | one line; `sqlx` only for `FromRow` users (P2-3) |
| 2. Configure and go | met (2026-09-23) | `#[lesto::main]` |
| 3. Very fast | half | 404 path 855 ns over axum (target met); happy path 308 ns over (not "within noise") |
| 4. Adopt from / leave for axum | friction | no `App::merge`/`nest_router`, custom extractors need an empty impl, no exit chapter (P3) |
| 5. Standard middleware | met for lesto's own | `lesto::layers` is public; the tower-http batteries are missing (P1, P3) |

Workstream B is complete; its numbers and the `Store::read` spike are in
[plan-B-performance.md](plan-B-performance.md).

---

# P0. Blockers before crates.io

Each of these either cannot be changed after publication without breaking users, or is the
first thing a stranger meets.

## P0-1. Key order is a guarantee: keep `preserve_order`, say so, test it

**Decision (2026-09-23, maintainer):** JSON keys keep declaration order, always — in the OpenAPI
document *and* in response bodies. `preserve_order` stays on `serde_json` and `schemars`.

**Why the earlier plan was wrong:** it proposed dropping `preserve_order` and restoring the order
by sorting or rebuilding the maps lesto emits. That cannot work: without the feature,
`serde_json::Map` is a `BTreeMap`, so schemars' output is already alphabetical when lesto
receives it; declaration order is lost before any code of ours runs. The same holds for response
bodies that go through `serde_json::Map` — problem extensions (`HttpError::extension`,
`Problem::extensions`) and any `Json<serde_json::Value>`. Typed structs serialized by serde
derive keep field order with or without the feature.

**The cost, accepted:** the feature is unified, so every crate in a dependent's graph gets an
`IndexMap`-backed `serde_json::Map` (insertion order instead of sorted order, slightly slower
lookups). Moving code into another crate would not scope it: feature unification is per graph.

**Tasks**
- [x] Record the decision in `AGENTS.md` "Design decisions", with the cost above, and replace the
      "`preserve_order` feature leak" mention in its roadmap paragraph.
- [x] `README.md` and tutorial appendix B ("Fields in the documentation are not in the struct's
      order"): state the guarantee and its side effect on the user's own `serde_json::Map`s.
      Mention that a `HashMap` field has no order to preserve (use `IndexMap` or `BTreeMap`).
- [x] Tests for the response side, next to `schema_properties_keep_declaration_order`: a typed
      response, a `Json<serde_json::Value>` response and a problem with several extensions keep
      their key order **in the raw body bytes** (not through `serde_json::from_slice` into a
      `Value`, which would hide a regression behind the same feature).
- [x] Replace `tokio/full` with the features lesto actually uses (`rt`, `rt-multi-thread`,
      `macros`, `net`, `signal`, `time`, `sync`). Verify with `cargo check -p lesto` for each
      feature alone and the examples.
- [x] Put `garde/email` and `garde/url` behind lesto features (`email`, `url`), on by default —
      batteries included — but switchable off, so regex and idna are not forced on every build.

**Done (2026-09-23):** tokio features `rt`, `rt-multi-thread`, `macros`, `net`, `signal`,
`time`; `cargo tree -e features -p hello` shows no `tokio/full`. The order tests fail with
`preserve_order` removed (checked) and pass with it.

**Done when:** order tests cover schema, typed body, `Value` body and problem extensions;
`cargo tree -e features -p hello` shows no `tokio/full`.

## P0-2. `IntoStatus for u16` must not panic

**Why:** `error.rs:31` is `StatusCode::from_u16(self).expect("invalid HTTP status code")`. A
handler doing `HttpError::new(upstream_status, ..)` with a value read from an upstream response
panics; the catch-panic layer turns it into an opaque `500`. `AGENTS.md` says "no panics on user
input" — this is the one place that breaks the rule.

**Tasks**
- [x] Map an out-of-range code to `500` and `tracing::error!` the offending value, or change the
      trait so only `StatusCode` and a fallible conversion are accepted. Prefer the first: it
      keeps `HttpError::new(404, ..)` ergonomic.
- [x] Unit test: `999`, `0` and `600` produce a `500` problem and a log record, not a panic.
- [x] Note the behavior in the `IntoStatus` rustdoc and in tutorial chapter 7.

**Done (2026-09-23):** outside `100..=599` (not only what `http` rejects: it accepts up to
999) → logged `500`; test covers 0, 99, 600, 999, 1000, `u16::MAX`.

**Done when:** no `expect`/`unwrap` is reachable from a status code value supplied at runtime.

## P0-3. `lesto new` and `lesto openapi`: implement or remove

**Why:** both exit 2 as "reserved". A stranger runs `lesto --help` before anything else.

**Tasks**
- [x] Implement `lesto openapi` (build the `App`, print `openapi_json()`; needs a way to reach the
      user's `App` — a `--bin` that prints on an env var or flag is enough), or remove it.
- [x] Remove `lesto new` from the CLI until it exists; keep it on the roadmap.

**Done (2026-09-23):** `lesto openapi [-p pkg] [-o file] [-- args]` builds the app and runs it
with `LESTO_OPENAPI_PATH`; `App::serve`/`serve_at`/`serve_until` write the document there and
return without binding. The app's stdout goes to stderr, 60 s timeout if `main` never reaches
`serve`. `lesto new` removed from the CLI, still on the roadmap.

## P0-4. Public API audit

**Why:** every public type is a promise from 0.1.0 on. Today only `db/settings.rs` uses
`#[non_exhaustive]`.

**Tasks**
- [x] Walk the public surface (`cargo doc` index). Mark `#[non_exhaustive]` on enums and
      config-like structs that will grow: `lesto::db::Error`, `lambda::Options`, `Trace`,
      `otel::Config`, `openapi` model types if they have public fields.
- [x] `#![warn(missing_docs)]` on `lesto`, `lesto-macros` exports; fix what it reports.
- [x] Re-export policy, written in `README.md` next to the axum policy (P5-4): every re-exported
      crate (`axum` types, and after P2 `tokio`, `serde`, `schemars`, `garde`, `sqlx`) is public
      API; a major release of any of them forces a lesto major.
- [x] `CHANGELOG.md` (Keep a Changelog format), starting at 0.1.0.

**Done (2026-09-23):** `#[non_exhaustive]` on `db::Error`, `Isolation`, `Rejection`,
`lambda::Options` (gained `Options::keep_stage(bool)`, since a struct literal no longer
compiles outside the crate), `otel::Config`, `DocsAssets` and every OpenAPI model type.
`Problem`, `ProblemError`, `ValidationErrorItem` stay exhaustive: their shape is the RFC 9457
wire format. `missing_docs` is on for both crates; `openapi.rs` allows it with a reason (the
fields are the specification's). README "Versioning" states the axum, re-export and MSRV
policies (P5-4's first two items), `CHANGELOG.md` added.

## P0-5. Freeze the breaking decisions before 0.1.0

**Why:** 0.x allows breaking changes, but each one is churn for the first users. These four
change signatures; decide them now, implement the ones that break.

**Tasks**
- [x] `sqlx::Error::RowNotFound` → implicit `404` or explicit `.or_not_found()` (question 8).
      Recommended: explicit. Implementation lives in P5-2 but lands before publishing.
- [x] `HttpError` field style (P5-3): pick accessors or public fields + builder, apply it.
- [x] `status = N` vs `Created<T>` (question 5, P5-1). Recommended: add the types, keep the
      attribute as a shortcut — not breaking, so only the decision is needed here.
- [x] Undocumented extractors (question 6, P3-4). Recommended: silent by default,
      `strict_docs()` for errors — not breaking, decision only.

**Done (2026-09-23), decisions by the maintainer:** all recommendations accepted.
`RowNotFound` is a 500 now; `lesto::db::NotFoundExt::or_not_found(detail)` (in the prelude) on
`Result<T, sqlx::Error>` and `Option<T>` marks the lookups that mean 404. `HttpError` uses
accessors everywhere: `status()` and `detail()` next to `type_uri()`, `title()`, `headers()`,
`extensions()`; the fields are private. `status = N` stays, `Created<T>` & co. come in P5-1;
undocumented extractors will be silent with `strict_docs()` (P3-4). Also decided at the same
time: garde's derive is **vendored** (P2-2), OpenTelemetry **metrics are implemented** (P4-1),
the shutdown deadline defaults to **30 s** (P1-1).

---

# P1. Robustness in production

What an operator hits in the first week. Small, and independent of the batteries work.

## P1-1. A deadline on graceful shutdown

**Why:** `App::serve_until` calls `axum::serve(..).with_graceful_shutdown(shutdown)` and nothing
else. One hung handler keeps the process alive until the orchestrator sends `SIGKILL`, which
turns a rolling deploy into dropped connections.

**Tasks**
- [x] `App::shutdown_timeout(Duration)`, default something sane (30 s matches Kubernetes' own
      grace period) or `None` to keep today's behavior — decide and write down which.
- [x] Implement by racing the server future against `tokio::time::sleep` after the shutdown
      signal fires; log at `warn` when the deadline is hit, with the number of in-flight requests
      if it is cheap to know.
- [x] Test in `tests/shutdown.rs`: a handler that never returns does not keep `serve_until` from
      resolving.
- [x] Tutorial chapter 11 ("Shutdown and panics") documents it.

**Done (2026-09-23):** `App::shutdown_timeout(impl Into<Option<Duration>>)`, default
`DEFAULT_SHUTDOWN_TIMEOUT` = 30 s. The in-flight count is not logged: axum does not expose it and
counting would add work to every request. Two tests: a hung handler with a 200 ms deadline, and
`None` still waiting.

## P1-2. Request timeout and body limit

**Why:** without a timeout a slow client or a stuck upstream holds a task forever; the body limit
is axum's implicit 2 MB, undocumented.

**Tasks**
- [x] Add `tower-http` (already in `Cargo.lock` transitively) with the `timeout` and `limit`
      features. Update `AGENTS.md`, which currently avoids tower-http.
- [x] `App::timeout(..)`: answers a `504` (or `503`) **problem**, not an empty body, through
      `HttpError`.
- [x] `App::body_limit(..)` sets `DefaultBodyLimit`; the 2 MB default is documented, and a
      `413` is a problem.
- [x] Tutorial chapter 11 and `README.md`. `cargo bench -p lesto`: no cost when unused.

**Done (2026-09-23), with one change of plan:** no tower-http. Its `TimeoutLayer` answers an
empty body, and wrapping it to produce a problem is more code than the layer itself, so
`lesto::layers::TimeoutLayer` is hand-written like the others (pin-projected, no boxing) and
answers **503**. `App::body_limit(impl Into<Option<usize>>)` installs axum's `DefaultBodyLimit`
(no new dependency either); the `413` was already a problem through `lesto::Json`. Both are a
`Router::layer` call only when set, so the request path of an application that sets neither is
unchanged (no bench needed: `into_router` differs by two `if`s at build time). tower-http comes
in with P3-2 for CORS and compression.

---

# P2. Batteries included (goal 1)

The target is this, for an application using SQLite, OpenAPI and validation:

```toml
[dependencies]
lesto = { version = "0.1", features = ["sqlite"] }
```

Today it needs five more lines. This block is what the whole positioning rests on: it is worth
more than any further performance work.

## P2-1. `#[lesto::main]`

**Why:** goal 2 says `main` stays one line; today that line needs `#[tokio::main]` and so tokio
in the user's manifest.

**Tasks**
- [x] `pub use tokio;` from lesto, with the feature set decided in P0-1.
- [x] `#[lesto::main]` in `lesto-macros`, expanding to
      `#[::lesto::tokio::main(crate = "::lesto::tokio")]` (`tokio-macros` 2.7.2 in `Cargo.lock`
      supports the `crate` option).
- [x] Accept the same options `tokio::main` does (`flavor`, `worker_threads`), pass them through.
- [x] `examples/01-hello` uses it; tutorial chapter 1 shows it; `README.md` example updated.
- [x] UI test: `#[lesto::main]` on a non-`async fn` produces a message that says what to do.

**Done (2026-09-23):** `#[lesto::main]` and `#[lesto::test]` (the second so tests need no tokio
either) expand to `#[::lesto::tokio::main(crate = "::lesto::tokio", ..)]`, options passed
through (tested with `flavor`/`worker_threads`). Every `#[tokio::main]`/`#[tokio::test]` in the
examples, the tutorial and the README now uses them; `examples/01-hello` depends on `lesto` alone.
Inside `lesto-macros` the unit tests spell `#[::core::prelude::v1::test]`, since `#[test]` now
names the crate's own macro.

## P2-2. `#[lesto::model]`

**Depends on:** nothing, but lands best next to P2-1.

**Why:** the derives a model needs (`Serialize`, `Deserialize`, `JsonSchema`, `Validate`) emit
absolute paths, which is why `serde`, `schemars` and `garde` must be direct dependencies.

**Tasks**
- [x] `#[lesto::model]` adds the derives and points them at lesto's re-exports:
      `#[serde(crate = "::lesto::serde")]`, `#[schemars(crate = "::lesto::schemars")]`.
      Add `pub use serde;` to lesto.
- [x] garde has no `crate` option (`garde_derive` 0.23 hard-codes `::garde::`). Decide, and write
      the decision here: (a) upstream PR adding `#[garde(crate = ..)]`, (b) document garde as the
      one remaining direct dependency until it lands, (c) vendor a patched derive. Recommended:
      (a) with (b) as the interim state.
- [x] One attribute: `#[lesto::model(views(Create(..), Update(..?)))]` (question 3,
      recommended). It removes the "`views` must sit above `#[derive]`" ordering rule, because the
      macro emits the derives itself. Keep `#[lesto::views]` working for hand-derived models.
- [x] Document in the tutorial where the derives come from, so the magic is legible.

**Done (2026-09-23):** garde's derive vendored (option c, the maintainer's choice) in
`lesto-macros/src/garde`, with `NOTICE.md` (provenance, license, changes, how to update).
`#[lesto::model]` emits the four derives through `lesto::serde` / `lesto::schemars` /
`lesto::__private::Validate`, `allow_unvalidated` when the type has no `#[garde]` at all, and
takes `views(..)`. `pattern` became a lesto feature too (implied by `email`, as in garde).
Tutorial chapters 4 and 6 and the README use it; the remaining snippets move in P2-4.

## P2-3. Re-export sqlx for `db` users

**Tasks**
- [x] Check whether `sqlx::FromRow`'s derive accepts a crate path. If it does, re-export sqlx as
      `lesto::db::sqlx` and have `#[lesto::model]` (or a `db` variant) point at it.
- [x] If it does not, document sqlx as a direct dependency of `db` users and say why, next to the
      garde note.

**Done (2026-09-23):** `sqlx-macros-core` 0.9 hard-codes `::sqlx::` (`derives/row.rs`) and has
no crate attribute, so `FromRow` users keep sqlx as a direct dependency; chapter 13 says why and
that `lesto::db::sqlx` (already re-exported) covers everything else. Vendoring it like garde was
not done: `FromRow` is optional (tuples work), and sqlx's macros are far larger than garde's.

## P2-4. One line in every manifest

**Depends on:** P2-1, P2-2, P2-3.

**Tasks**
- [x] `examples/01-hello`, `02-notes`, `03-lambda`, `04-opentelemetry`, `05-routers`,
      `99-tutorial`: remove every dependency that lesto can now provide.
- [x] Tutorial chapter 1 and `README.md`: the install snippet is one line.
- [x] Whatever cannot be removed (garde until P2-2 lands, sqlx, tracing) gets one sentence saying
      why it is still there.

**Done when:** `examples/01-hello/Cargo.toml` lists `lesto` and nothing else.

**Done (2026-09-23):** every example's `[dependencies]` is `lesto` alone, except `02-notes` and
`04-opentelemetry`, which keep `sqlx` for `#[derive(sqlx::FromRow)]` (P2-3). `tracing` is
re-exported as `lesto::tracing` (added to the re-export list in the README). All derives in the
examples, the tutorial and the README went through `#[lesto::model]`; appendix B now points at
it for "cannot find garde". Dev-dependencies (`tower`, `http-body-util`) are unchanged: they are
test tooling, not part of the one-line claim.

---

# P3. axum interoperability, middleware batteries (goals 4 and 5)

## P3-1. Mount an existing `axum::Router`

**Why:** goal 4's adoption path. Today it is `map_router(|r| r.merge(other))`, which nobody finds.

**Tasks**
- [x] `App::merge(router)` and `App::nest_router(prefix, router)`; the routes they bring are not
      documented in OpenAPI, which is stated in the rustdoc.
- [x] Consider `impl From<Router<S>> for App<S>`.
- [x] Tutorial chapter 11 gains the "coming from axum" direction next to the existing exit path.

**Done (2026-09-23):** `App::merge(Router<S>)`, `App::nest_router(prefix, Router<S>)` and
`impl From<Router<S>> for App<S>`; merged routes go through the problem, panic, span and timeout
layers (tested). Chapter 11 opens its axum section with "Coming from axum".

## P3-2. The rest of the tower-http batteries

**Depends on:** nothing; P1-2 did not add tower-http after all, this task does.

**Tasks**
- [x] Add `tower-http` with the features used; update `AGENTS.md`, which currently avoids it.
- [x] `App::cors(..)`, `App::compression()`, `App::request_id()`. Each installs the tower-http
      layer and stays usable directly through `App::layer` or on a plain `axum::Router`.
- [x] Tutorial chapter 11 documents the new builder methods; `README.md` "What the framework
      does" mentions them.
- [x] Re-run `cargo bench -p lesto`: the defaults must not cost anything when unused.

**Done (2026-09-23):** tower-http 0.6 with `cors` and `request-id` always, `compression-gzip`
behind lesto's `compression` feature (default on: flate2 is the only heavy part).
`App::cors(CorsLayer)` (tower-http's type, re-exported as `lesto::cors`), `App::compression()`,
`App::request_id()`, each an opt-in `Router::layer` outside the shared stack, so the request path
of an app that uses none of them is unchanged.

## P3-3. Warn when `axum::Json<T>` hides validation

**Why:** `axum::Json<T>` with `T: Validate` documents fine and validates nothing, silently
(review §4.3).

**Tasks**
- [ ] Emit a deprecation-style warning from the route macro when the argument is
      `axum::Json<T>` / `axum::extract::Query<T>` and `T: Validate`, naming `lesto::Json`.
- [ ] Keep the impls: removing them would break goal 4.

## P3-4. Accept undocumented custom extractors

**Why:** today an axum extractor that lesto does not know fails to compile until the user writes
`impl OperationInput for X {}`. That empty impl is a tax on the adoption path.

**Tasks**
- [ ] Autoref specialization in the route macro: call `describe` when an `OperationInput` impl
      exists, do nothing otherwise.
- [ ] Default per P0-5 (recommended: silent, `strict_docs()` for teams that care).
- [ ] Update the UI tests and the table in `AGENTS.md` / `docs/tutorial/B-common-problems.md`.

## P3-5. Tutorial chapter: leaving lesto

**Depends on:** P3-2 (so the chapter can list the layers a leaver keeps).

**Tasks**
- [ ] New chapter showing the exit step by step: `into_router()`, the layers from
      `lesto::layers`, `lesto::Json`/`Query`, `HttpError` on a plain `axum::Router`.
- [ ] Compile the snippets in `examples/99-tutorial`.
- [ ] Link it from `README.md`: "nobody is locked in" is an adoption argument, not an admission.

---

# P4. Observability and positioning decisions

## P4-1. Metrics: wire them or say they are out of scope

**Why:** the `otel` feature exports **traces and logs only** (`opentelemetry` features `trace`,
`logs`). A production service wants request rate, error rate and duration as metrics; today they
have to be derived from spans in the backend, and the tutorial chapter is called
"Observability" without saying so.

**Tasks**
- [ ] Decide: (a) add `opentelemetry/metrics` plus a meter provider in `otel::install` and a
      small set of HTTP server metrics from the request layer (`http.server.request.duration`
      at least), or (b) declare metrics out of scope and point at
      `tracing-opentelemetry`/`metrics-rs`.
- [ ] Whichever is chosen, chapter 15 says it in one paragraph, near the top.
- [ ] If (a): the metrics must cost nothing when no meter provider is installed, and
      `cargo bench -p lesto` must show it.

## P4-2. `otel` in the default features, with numbers

**Recommended:** off by default; measure to confirm.

**Tasks**
- [ ] Measure a clean and an incremental build of `examples/02-notes` with and without `otel`.
- [ ] Decide (question 4) and record the numbers in `README.md` next to the performance table.

---

# P5. API polish, database ergonomics, publication

## P5-1. Response types for statuses

**Why:** `status = N` rewrites *any* `200` into `N` at runtime, so a route declared `201` can
never answer `200` on purpose, and the status does not come from the type (review §4.2).

**Tasks**
- [ ] `Created<T>`, `Accepted<T>`, `NoContent` implementing `IntoResponse + OperationOutput`.
- [ ] `status = N` stays as a shortcut (per P0-5).
- [ ] Tutorial chapter 6 and the attribute table in `README.md`.

## P5-2. `lesto::db` ergonomics and documentation

**Tasks**
- [x] `RowNotFound`: implement the P0-5 decision (**before** P5-5). Done in P0-5.
- [ ] Tutorial: implementing `Requirement<P>` for a user-defined enum, so permissions are not
      only strings.
- [ ] Tutorial FAQ: "how do I call two store methods atomically?" → the `..._in(conn, ..)`
      convention, and why two public store methods sharing a transaction stays impossible.
- [ ] Tutorial: migrations (`sqlx::migrate!`), pool sizing, acquisition timeouts, and the read
      replica — the production knobs chapter 13 never mentions.
- [ ] Document that `Authenticated::State` is an associated type, so a principal belongs to one
      state type.

## P5-3. `HttpError` field style

- [x] Implement the P0-5 decision (**before** P5-5): public `status`/`detail` next to private
      boxed extras is two styles in one type. Done in P0-5: accessors everywhere.

## P5-4. README positioning and policies

- [ ] Goals first: "batteries included, like FastAPI", then the comparison table from review §11.
- [ ] axum version policy, stated plainly: "lesto X.Y tracks axum 0.8; an axum 0.9 release forces
      a lesto major" (review §4.6), next to the re-export policy from P0-4.
- [ ] MSRV 1.94 with its reason (sqlx 0.9).
- [ ] `scalar_script_url` / `swagger_ui_base_url` mentioned near the docs section, for offline or
      firewalled deployments.
- [ ] OTel auto-init and the subscriber escape hatch on the first page.

## P5-5. Publication

**Depends on:** P0, and ideally P1 and P2.

- [ ] `cargo publish --dry-run` for `lesto-macros`, then `lesto`, then `lesto-cli` (the manifests
      are ready; that is the order).
- [ ] `cargo deny check` locally (it is CI-only today; `cargo install cargo-deny`), including the
      licenses of anything P1–P3 add.
- [ ] Decide the version: `0.1.0` with the axum and re-export policies above.

---

# Open questions

All answered by the maintainer on 2026-09-23.

1. **garde crate path**: vendor a patched derive. (P2-2)
2. **sqlx derive path**: investigate in P2-3; sqlx is already re-exported as `lesto::db::sqlx`.
3. **`#[lesto::model]` and `#[lesto::views]`**: one attribute with `views(..)`. (P2-2)
4. **`otel` in default features**: off, numbers recorded in P4-2.
5. **Status**: `Created<T>` alongside `status = N`. (P5-1)
6. **Undocumented extractors**: silent, error behind `strict_docs()`. (P3-4)
7. **`db` reads without a transaction**: keep the transaction (plan B, B10).
8. **Implicit `RowNotFound` → 404**: removed, explicit `.or_not_found()`. (P0-5)
9. **Name of the span layer**: `RequestSpanLayer`.
10. **JSON key order**: always declaration order, `preserve_order` kept. (P0-1)
11. **Metrics**: implemented. (P4-1)
12. **Shutdown deadline**: 30 s by default. (P1-1)

# Deliberately not on this list

Out of scope unless asked (from `AGENTS.md`): actix-web backend, websockets, multipart, typed
header/cookie parameters beyond API keys, an ORM or query builder, role hierarchies or permission
wildcards, `sqlx::Any`, hot patching without restart. Further performance work is also off the
list: the remaining 308 ns on the happy path are the span and axum's own boxing, and the next
gain would cost more in complexity than it returns.

**A separate `lesto-db` crate** was considered on 2026-09-23 and is not needed: it would not
scope `preserve_order` (feature unification is per dependency graph, not per crate), and the
`db` feature already keeps sqlx out of builds that do not ask for it. Revisit only if `lesto::db`
needs its own release cadence.

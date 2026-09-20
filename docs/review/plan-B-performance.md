# Workstream B: performance, task list

*Created 2026-09-18 from section 10.B of [the pre-publication review](2026-09-pre-publication-review.md)
(items 4–9, plus A3, which concerns the same middleware). Goal 3: framework overhead over plain
axum within measurement noise on the happy path, and under 1 µs on the 404 path.*

## Baseline (review, section 3.1)

| Scenario | axum 0.8 | lesto (default) | lesto `Trace::off()` |
|---|---:|---:|---:|
| `GET /hello` | ~620 ns | ~1,510 ns | ~1,140 ns |
| `POST /items` (JSON + validation, `status = 201`) | ~1,200 ns | ~2,200 ns | ~1,820 ns |
| `GET /missing` (404) | ~380 ns | ~2,300 ns | ~1,850 ns |

## Baseline from `benches/overhead.rs` (B0)

`cargo bench -p lesto`, Apple M-series laptop, `LESTO_BENCH_ITERS=60000 LESTO_BENCH_ROUNDS=3`,
best round. The axum column is the same request through a plain router, not the same answer:
axum does not validate, so the invalid body is a `201` there.

| Scenario | axum 0.8 | lesto | lesto `Trace::off()` |
|---|---:|---:|---:|
| `GET /hello` | 622 ns | 1,546 ns | 1,132 ns |
| `POST /items` (JSON + validation, `status = 201`) | 1,193 ns | 2,241 ns | 1,791 ns |
| `POST /items` invalid body (422) | 1,182 ns | 4,851 ns | 4,400 ns |
| `GET /missing` (404) | 388 ns | 2,294 ns | 1,902 ns |
| `GET /err` (handler `HttpError`, 404) | 827 ns | 2,232 ns | 1,832 ns |
| `GET /missing` 404, `ErrorFormat::FastApi` | 388 ns | 2,391 ns | — |
| `POST /items` invalid, `ErrorFormat::FastApi` | 1,182 ns | 5,958 ns | — |

## Result (2026-09-20, after B1-B9)

Same machine and harness as the baseline above.

| Scenario | axum 0.8 | lesto before | lesto now | lesto `Trace::off()` |
|---|---:|---:|---:|---:|
| `GET /hello` | 616 ns | 1,546 ns | **911 ns** | 864 ns |
| `POST /items` (JSON + validation, `status = 201`) | 1,187 ns | 2,241 ns | **1,498 ns** | 1,477 ns |
| `POST /items` invalid body (422) | 1,176 ns | 4,851 ns | **3,007 ns** | 2,971 ns |
| `GET /missing` (404) | 388 ns | 2,294 ns | **1,260 ns** | 1,230 ns |
| `GET /err` (handler `HttpError`, 404) | 805 ns | 2,232 ns | **1,215 ns** | 1,195 ns |
| `GET /missing` 404, `ErrorFormat::FastApi` | 388 ns | 2,391 ns | **1,195 ns** | — |
| `POST /items` invalid, `ErrorFormat::FastApi` | 1,176 ns | 5,958 ns | **3,273 ns** | — |

(The two `ErrorFormat::FastApi` rows are history: the format was removed on 2026-09-20, so the
benchmark no longer has those cases.)

Against goal 3: the 404 path is 872 ns over axum (**under 1 µs**, B2's target) and the happy
path 295 ns, which is not yet "within measurement noise" — what is left there is the request
span and the two extra services, not work that can be deleted. The `422` path stays the most
expensive, and has no axum number to compare with: plain axum does not validate.

## Order and dependencies

```
B0 benchmarks ──┬─> B1 problem rendering ──> B2 problem layer ──┐
                ├─> B3 request span layer ──────────────────────┤
                ├─> B4 catch-panic layer ───────────────────────┼─> B8 docs + AGENTS.md
                ├─> B5 WithStatus future                        │
                ├─> B6 docs routes as Bytes                     │
                └─> B7 OTel propagation gate ───────────────────┘
B9 principal cache       (independent, `db`)
B10 reads without a transaction   (spike only; decision needed, review §9 question 7)
```

B0 comes first, so every other task can show its before/after numbers. B1–B7 are independent
of each other after B0 (except B2, which depends on B1) and can land as separate commits in any order.

Every task ends with the gate in AGENTS.md: `cargo test --workspace`, clippy with
`--all-features`, `cargo fmt --all`, rustdoc with `-D warnings`, the per-feature `cargo check`
commands, and `git grep -i presto`.

---

## B0. Benchmarks in the repository

**Why:** every other task needs a before/after number, and goal 3 needs a guard against
regressions.

**Tasks**
- [x] Add `crates/lesto/benches/overhead.rs` (`harness = false`), porting the appendix benchmark
      of the review. Cases:
  - `GET /hello`: axum vs lesto vs lesto `Trace::off()`;
  - `POST /items` (JSON + validation, `status = 201`);
  - `GET /missing` (404);
  - `GET` on a route that returns `HttpError::not_found` (handler error path);
  - `POST` with an invalid body (422 path);
  - `ErrorFormat::FastApi` on the 404 path.
- [x] Choose the harness. **Decided: hand-written, no dev-dependency.** Warm-up, N iterations,
      best of R rounds, tunable with `LESTO_BENCH_ITERS` / `LESTO_BENCH_ROUNDS`. criterion and
      gungraun both buy statistics that a best-of already approximates here (every disturbance
      makes a round slower, never faster), against AGENTS.md's "prefer no new dependencies".
      Revisit if CI ever gates on the numbers: that is where instruction counts would earn their
      dependency.
- [x] Add `scripts/bench-http.sh`: start `examples/01-hello` in release mode and run `oha` for
      throughput and latency percentiles, as a real-network check.
- [x] CI: already covered. `cargo clippy --workspace --all-targets` in the lint job compiles
      the benchmarks (and lints them), so no step was added. No regression threshold: the
      numbers are not stable enough on a shared runner to gate on.
- [x] Record the baseline numbers from the new harness at the top of this file.

**Done when:** `cargo bench -p lesto` prints all cases, and the numbers are recorded here.

---

## B1. Render problems once, with `instance` and format known at render time

**Why:** `finish_problem` (`app.rs:608`) buffers every `application/problem+json` body, parses it
back into a `Problem`, and serializes it again, only to fill `instance` and apply
`ErrorFormat::FastApi`. This is most of the 404 and 422 cost.

**Chokepoint:** every lesto error goes through `impl IntoResponse for Problem`
(`error.rs:108`): `HttpError`, `ValidationError`, `Rejection`, and `lesto::db::Error`
(`db/error.rs:216`) all end there.

**Design (preferred): request-scoped render context**
- The problem layer (B2) runs the inner service inside a `tokio::task_local!` scope holding
  `{ instance: the request path, format: ErrorFormat }`.
- `Problem::into_response` reads the task-local when present. It fills `instance` if missing,
  renders in the right format (RFC 9457 or FastAPI, with the matching `Content-Type`), and
  serializes **once**. It marks the response with a zero-sized extension (`ProblemRendered`).
- Outside the layer (a plain `axum::Router`, goal 4's exit path) the task-local is absent. The
  response is then RFC 9457 without `instance`, as today. Nothing breaks.
- The request path is copied into the context once per request. It can be an `Arc<str>` or
  the `Uri` itself; measure whether cloning the `Uri` (reference-counted) beats copying the path.

**Alternative (if the task-local measures badly):** put the `Problem` in the response
extensions with an empty body, and let the layer serialize. Rejected by default because
responses built outside the layer would have no body.

**Tasks**
- [x] Add the render context (`error.rs` or a new `problem.rs`), plus `ProblemRendered`.
- [x] Move FastAPI rendering (`problem_to_fastapi`) into the render path.
- [x] Unit tests: rendering with and without the context; FastAPI format; `instance` already set
      by the user is kept.
- [x] Check that problems created in a spawned task (no context) still render correctly.

**Done when:** no lesto-produced error is parsed back from JSON anywhere.

---

## B2. `ProblemLayer`: a public tower layer replacing `finish_problem`

**Depends on:** B1.

**Tasks**
- [x] Hand-written `ProblemLayer` / `ProblemService<S>` with a pin-projected future
      (`pin-project-lite`, already in the lock file through tokio and hyper). No `from_fn`, no
      boxing.
- [x] The service sets the B1 context around `inner.call(req)`, which requires wrapping the future
      in the task-local scope. `tokio::task::futures::TaskLocalFuture` is a concrete type: no box.
- [x] Slow path, kept for compatibility: a `problem+json` response **without** `ProblemRendered`
      (built by hand by user code) is still parsed and rewritten as today. Tested explicitly.
- [x] Replace the `middleware::from_fn(finish_problem)` in `into_router` (`app.rs:421`).
- [x] Export it publicly (`lesto::layers::ProblemLayer` or similar) with rustdoc showing its use
      on a plain `axum::Router`. This is the first public layer of workstream D; agree the module
      name there.
- [x] Integration tests: 404, 405, handler `HttpError`, extractor 422, panic 500, all with and
      without `ErrorFormat::FastApi`; `instance` present in each.

**Done when:** `GET /missing` overhead over axum is under 1 µs in B0.

---

## B3. Request span as a hand-written layer

**Why:** the trace `from_fn` costs about 370 ns with no subscriber installed, while the docs
claim "one atomic load" (review A3).

**Tasks**
- [x] Hand-written `Layer`/`Service` with a pin-projected future that holds the `Span`, enters it
      on each poll (`Instrumented`-style), and records the status on completion. `pin-project-lite`
      is the one dependency this workstream added (macro-only, already in the tree under tokio).
- [x] Fast path: when `span.is_disabled()`, the service calls the inner service directly and
      returns its future unchanged (an enum future with a `Disabled` variant), so the cost
      really is one callsite check.
- [x] Keep the placement: added as the last `Router::layer`, so `MatchedPath` is available and
      the span wraps the problem and panic layers.
- [x] Name: avoid a clash with `tower_http::trace::TraceLayer` (review §9 question 9). Proposal:
      `RequestSpanLayer`.
- [x] Export it publicly, with a rustdoc example on a plain `Router`.
- [x] `tests/trace.rs` passes unchanged (span fields, names, status).
- [x] Fix the module docs (`trace.rs`): the "one atomic load" sentence must match the measured
      cost after this task.

**Done when:** default vs `Trace::off()` differ by less than 50 ns on `GET /hello` with no
subscriber installed. **Met: 47 ns.** The bigger finding was next door: every `Router::layer`
call makes axum re-box each route *and its future*, so the three layers now go on in one call
through `tower_layer::Stack`, which is most of the 635 ns won on `GET /hello`. `Trace::off()`
therefore no longer removes a layer; it takes a branch inside one that is installed anyway.

---

## B4. Catch-panic without per-request boxing

**Why:** `CatchPanic` (`app.rs:663`) boxes twice per request.

**Finding that changes the review's proposal (§3.3.3):** `tower_http::catch_panic` 0.6 returns
`Response<UnsyncBoxBody<Bytes, BoxError>>` (`catch_panic.rs:193`). It re-boxes every response
body, and axum then wraps it in `Body` again. Switching to it would *add* an allocation per
request. tower-http can still come in for CORS and compression (workstream D), but probably not
for this.

**Tasks**
- [x] Rewrite lesto's catcher as `CatchPanicLayer` with a pin-projected future: `catch_unwind`
      around `call` and around each `poll`, and the `500` problem built by B1's rendering.
      No boxing.
- [~] Benchmark it against `tower_http::catch_panic` in B0: **not done, and not worth doing.**
      The finding is in its type signature (`Response<UnsyncBoxBody<Bytes, BoxError>>`), so the
      extra allocation per request is not in question; measuring it would mean a dev-dependency
      on tower-http for a number nobody will act on.
- [x] Export it publicly next to `ProblemLayer`.
- [x] Update AGENTS.md ("the panic catcher is hand-rolled"): the reason is now the body boxing,
      not avoiding a dependency.
- [x] Test: panic while building the future and panic while polling both answer a `500` problem
      with `instance`.

---

## B5. `WithStatus` with a concrete future

**Why:** `route.rs:1307` boxes a future on every request to routes with `status != 200`.

**Tasks**
- [x] `type Future = WithStatusFuture<H::Future>` (pin-projected), rewriting `200` to the declared
      status on completion.
- [x] Keep the behavior identical (the semantics question is workstream E, review §4.2).

**Done when:** `POST /items` (`status = 201`) shows no overhead compared with the same route at
`status = 200`.

---

## B6. Docs routes served from `Bytes`

**Why:** `/openapi.json`, `/docs`, and `/swagger` copy the whole document or page on every
request (`app.rs:373`, `:393`, `:407`).

**Tasks**
- [x] Build `Bytes` once in `into_router`; each handler returns a clone (a reference-count
      increment) with the right `Content-Type`.
- [x] Optional, cheap: `ETag` computed once, and `304` on `If-None-Match`. Done, with a test.

---

## B7. OpenTelemetry propagation only when OpenTelemetry is active

**Why:** with the `otel` feature on, `propagation::set_parent` (`trace.rs:395`) queries the
global propagator on every request, even when no exporter was installed.

**Tasks**
- [x] A `static` `AtomicBool` set by `otel::init` / `init_named` / `auto_init` when they install
      the pipeline. `set_parent` returns immediately when it is `false`.
- [x] Applications with their own `tracing-opentelemetry` setup (the escape hatch) need
      propagation too. Expose `lesto::otel::enable_propagation()`, or detect it through the
      subscriber (`tracing_opentelemetry::OpenTelemetrySpanExt::set_parent` returning `Err`).
      Decide which, and document it in chapter 15.
- [~] B0 case with the `otel` feature on and no endpoint: the benchmark already runs with the
      feature on (lesto's dev-dependency on itself enables everything), and the gate does not
      show up there — with no subscriber the span is disabled and `set_parent` was never
      reached. The gate pays off in an application that *has* a subscriber but exports nothing.

---

## B8. Documentation and records

**Depends on:** B1–B7.

- [x] Update the benchmark table in this file and in the review with the final numbers.
- [x] AGENTS.md: the new layers in "Layout" and "How lesto works" (render context,
      `ProblemRendered`, slow path); the catch-panic rationale (B4).
- [x] Tutorial chapter 11 (middleware and axum): the public layers on a plain `Router`, with
      the snippet compiled in `examples/99-tutorial`.
- [x] README: a short "Performance" note with the method and a link to `benches/`.

---

## B9. `db`: authenticate once per request

**Why:** each store argument authenticates the principal again (`db/principal.rs`,
`Principal::extract`). Two stores in one handler verify the token twice.

**Tasks**
- [x] After `authenticate` succeeds, store the principal in `Parts::extensions` keyed by type.
      On the next extraction, reuse it.
- [x] Choose the sharing mechanism:
  - require `Clone` on `Authenticated` (simple; users' principals are usually small);
  - or keep an `Arc<P>` in the extensions and in `Store`. `principal()` keeps returning `&P`;
    `into_principal()` needs `Arc::try_unwrap` or a changed signature.
  Recommendation: `Arc<P>`, so no bound is added to every user's principal.
- [x] Do not cache failures: a `401` is returned as today.
- [x] Test: a handler with two stores calls `authenticate` exactly once (a counter in the test
      state).
- [x] Tutorial chapter 13: one sentence on the caching.

---

## B10. Spike: `Store::read` without a transaction

**Status:** measured on 2026-09-20; **the decision is still the maintainer's.** No
implementation.

Measured with a throwaway harness (a `#[ignore]`d test, not kept): `SELECT 1` three thousand
times through `Store::read` against the same statement on a pooled connection. Postgres 18 in
Docker on macOS, SQLite in memory.

| | through `read` | single statement | difference |
|---|---:|---:|---:|
| Postgres, p50 | 648 µs | 394 µs | +254 µs (+64%) |
| Postgres, p99 | 738 µs | 464 µs | +274 µs |
| SQLite, p50 | 59 µs | 38 µs | +21 µs (+56%) |
| SQLite, p99 | 129 µs | 90 µs | +39 µs |

The Postgres figure is three round trips (`BEGIN READ ONLY`, the statement, `COMMIT`) against
one, on a connection whose round trip is expensive here (Docker on macOS); on a unix socket the
absolute numbers fall a lot, the *ratio* of round trips does not. SQLite pays no network, and
still pays about half as much again to open and close the transaction.

What is lost by dropping the transaction for single-statement reads:

- `READ ONLY` enforcement — the closure could write, and the type system would not know;
- a consistent snapshot across several statements in one closure, which is the point of taking
  a closure at all, and cannot be decided from inside `read` (the closure is opaque);
- `TransactionSettings` (`SET LOCAL`), which row level security depends on: without a
  transaction the setting would leak to the next user of that pooled connection, which is a
  security bug, not a slow path. A principal with settings must keep the transaction.

**Recommendation: do not do it as a default.** The cost is real but it is one round trip's
worth on a path that already makes one, and the three things above are the guarantees the
`lesto::db` design is sold on. If the number matters for a specific read, the escape hatch that
fits the design is an explicit opt-in — a `read_unscoped` (or `Db::acquire`) that does not
begin a transaction, refuses to compile for a principal with settings, and says in its name
that there is no snapshot — rather than making `read` quietly weaker. That is a new API and so
a decision, not a performance fix.

# Background tasks

Status: proposed on 2026-11-14, implementation in progress.

This document is the written design of work a handler starts and that runs after the response
has been sent: FastAPI's `BackgroundTasks`, Vercel's and Cloudflare's `waitUntil`. It builds on
the internal Lambda extension that flushes telemetry after the response (`lesto::lambda`,
decided 2026-11-13, issue #4).

## Goal

```rust
use lesto::prelude::*;

#[lesto::post("/orders", status = 201)]
async fn create(tasks: BackgroundTasks, Json(order): Json<Order>) -> Json<Created> {
    let created = save(&order).await;
    tasks.spawn(async move { send_confirmation(&order).await });
    Json(created)
}
```

The caller gets `201` as soon as the handler returns. `send_confirmation` runs afterwards, on
every deployment target lesto supports, with one guarantee that the target can actually keep:
the task runs to completion unless the process dies or a deadline ends it first.

## Non-goals

- **Durability.** A task lives in the memory of one process. A crash, an out-of-memory kill or
  a Lambda timeout loses it, and nothing retries it. Work that must happen belongs in a queue
  (SQS, a database outbox). The documentation says so in the first paragraph, not in a footnote.
- **Results.** A task returns nothing to the request. There is no handle to await, cancel or
  inspect: the request is over by the time the task runs.
- **Scheduling.** No delays, no cron, no priorities, no concurrency limits.
- **wasm32.** `BackgroundTasks` is not compiled on `wasm32-unknown-unknown` (no `tokio::spawn`
  there). Cloudflare's `ctx.wait_until` is the natural backend later, behind the same API.

## API

`lesto::BackgroundTasks`, re-exported in the prelude. Not behind a feature: it needs only
`tokio`'s `rt`, which lesto already depends on.

```rust
#[derive(Clone)]
pub struct BackgroundTasks { /* where tasks go for this request */ }

impl BackgroundTasks {
    /// Run `task` after the response. Returns immediately.
    pub fn spawn<F>(&self, task: F)
    where
        F: Future + Send + 'static,
        F::Output: TaskOutput;
}

/// What a background task may return: `()`, or a `Result` whose error is logged.
pub trait TaskOutput: Send + 'static { /* sealed-ish: implemented by lesto */ }
impl TaskOutput for () {}
impl<E: std::fmt::Display + Send + 'static> TaskOutput for Result<(), E> {}
```

- **Extractor.** `FromRequestParts<S>` for every `S`, rejection `Infallible`. `OperationInput`
  with an empty `describe`: it adds nothing to the OpenAPI document. It is not a body extractor,
  so it can sit in any position.
- **`spawn`, not `add_task`.** FastAPI's name takes a function and its arguments; Rust has
  `async move` blocks, and `spawn` is what Rust users already call this. The task starts
  immediately on the runtime (it is not deferred until the response is written): on a server
  the difference is invisible, and deferring would mean holding futures in the request.
- **Errors.** A task returning `Err(e)` logs `e` at `ERROR`, target `lesto::background`, inside
  the task's span. Errors are the application's own type, so lesto only needs `Display`.
- **Panics.** Caught around `poll` with the same hand-written `catch_unwind` future the panic
  layer uses (no `futures-util`), logged at `ERROR` with the payload as text, never propagated.
  A panic must still mark the task as finished, or a Lambda invocation would wait for it until
  its deadline: the tracker's guard is decremented on drop.
- **Many tasks per request** are fine; they run concurrently, not in order. A handler that
  needs order writes one task with sequential `.await`s.

## Tracing

Each task runs in a span `background {name}` (`otel.name`), `otel.kind = "internal"`, child of
the span current when `spawn` was called, which is the request span in a handler. The parent
span closes before the child: OpenTelemetry allows it, and every backend draws it as work after
the response, which is what happened. `{name}` comes from the future's type name, read with the
helper the store spans already use (`db::trace::operation_of`), moved out of `db` into
`trace.rs` so it exists without the `db` feature. An `async move` block written in `create` is
spelled `..::create::{{closure}}::{{closure}}`, so the span is `background create` (with its
module: `background handlers::create`); anything else falls back to `background`.

## Where tasks run

A `BackgroundTasks` value holds a `Tracker`: an `Arc` with an `AtomicUsize` of live tasks and a
`tokio::sync::Notify`, hand-written (`tokio-util`'s `TaskTracker` would add a dependency for
forty lines). `spawn` increments, `tokio::spawn`s the wrapped future, and the wrapper's guard
decrements and notifies on drop. `Tracker::wait` returns once the count is zero.

The extractor looks for a per-request tracker in the request extensions (`BackgroundScope`,
crate-private) and falls back to a process-wide one (`static`). Nothing is inserted per request
on a server, so an application that never names `BackgroundTasks` pays nothing.

### Server (`App::serve*`)

The process-wide tracker. Tasks run on the server's runtime. `serve_until` waits for it after
the server has stopped (all in-flight requests done), inside the **same** `shutdown_timeout`
deadline that already bounds the requests: an orchestrator's grace period does not grow because
tasks exist. Tasks still running at the deadline are abandoned with a `WARN` naming how many.
A server with no shutdown timeout (`shutdown_timeout(None)`) waits for them without a bound.

### Lambda, one invocation at a time

Lambda freezes the execution environment when the runtime *and* every registered extension have
asked for their next event. The internal extension already exists for the telemetry flush; it
becomes the general "after the response" mechanism:

1. `invoke_once` creates a `Tracker` per invocation and inserts it as `BackgroundScope` in the
   request extensions, so the handler's `BackgroundTasks` points at it.
2. It runs the router and collects the body, as today.
3. Instead of signaling "done" when `invoke_once` returns, it moves the `DoneOnDrop` guard into a
   spawned waiter: `tracker.wait()` (bounded, see below), then drop the guard. The response goes
   back to the runtime immediately.
4. The extension receives "done", flushes the telemetry (which now includes the tasks' spans and
   logs), and only then asks for its next event. Lambda freezes.

The wait is bounded by the invocation's deadline (`lambda_http`'s `Context::deadline`) minus a
margin of 500 ms, so the extension still has time to flush and to log a `WARN` that the
invocation ended with tasks still running. Tasks past the deadline are not aborted: they are
frozen with the environment and resume on the next thaw, or die with it. Cancelling them at a
point of the application's choosing would be worse than letting them continue.

**The extension is now registered whenever lesto runs in Lambda with one invocation at a time**,
not only when there is telemetry to flush: whether the application spawns tasks is not known at
init, and extensions can only register during init. Without tasks and without telemetry the
waiter signals at once, and the cost is one local `event/next` round trip per invocation, inside
the time Lambda bills anyway. It is renamed from `lesto-telemetry-flush` to `lesto`.

**No `reqwest` for the extension.** Its two calls move to `lambda_runtime_api_client::Client`,
the hyper-util client the runtime itself uses (already in the graph through `lambda_http`, added
as a direct dependency of the `lambda` feature at the version `lambda_http` 1.3 brings), so the
extension exists with `lambda` alone, without `otel`. The flush step stays `#[cfg(feature =
"otel")]`.

### Lambda: fallbacks

- **No Extensions API** (an emulator without it): `invoke_once` waits for the tasks *before*
  returning the response, as the telemetry flush already does. Correct, slower for the caller.
  The init warning is reworded: it is no longer only about telemetry.
- **Managed concurrency** (`AWS_LAMBDA_MAX_CONCURRENCY` > 1): no freeze between invocations and
  no pairing of `INVOKE` events with responses. No extension, no per-invocation scope: tasks use
  the process-wide tracker and run like on a server. Best effort at shutdown.
- **`lambda::test::invoke`**: waits for the tasks before returning, so a test can assert their
  effects right after the call.

### MCP

A tool call is an inner request to the app's own router (`mcp::dispatch`). That request is new,
so it does not carry the outer request's extensions. `dispatch` copies `BackgroundScope` from the
outer request to the inner one, or a tool's tasks in Lambda would land on the process-wide
tracker and be frozen mid-flight.

### Tests through `oneshot`

The process-wide tracker: tasks run on the test's runtime. A test that asserts a task's effect
needs to wait for it; `lesto::test`-style helpers are out of scope for now; the tutorial shows
waiting on a channel the task writes to.

## Billing and capacity (documented, not solved)

- In Lambda the time after the response is billed (`PostRuntimeExtensionsDuration`) and counts
  against the function timeout.
- Lambda does not send the next invocation to an environment whose extension is busy. Slow tasks
  lower the throughput of each environment, so Lambda scales out: more environments, more cold
  starts. The telemetry flush already does this; tasks can make it much larger.

## Documentation

- New tutorial chapter `17-background-tasks.md` (appended to keep chapter numbers stable):
  the API, errors and panics, tracing, server shutdown, testing, and the non-goals first.
- Chapter 14 (Lambda): a section on tasks after the response, the deadline, billing, and the
  fallbacks.
- Appendix A (FastAPI): `BackgroundTasks` mapping.
- Snippets in `examples/99-tutorial`, with tests for the runtime claims.
- `AGENTS.md`: layout entry, design decision paragraph, the extension renamed.
- `CHANGELOG.md` under `[Unreleased]`.

## Tests

- `tests/background.rs`: a task runs after the response; `Err` and panics are logged and do not
  affect the response; several tasks; `serve_until` waits for a task during shutdown and gives
  up at the deadline.
- `tests/lambda.rs` / `lambda.rs` unit tests: `test::invoke` waits for tasks; with the stand-in
  Extensions API, "done" is signaled only after the task finishes and the response was returned
  before that; the deadline bound; no Extensions API means tasks before the response.
- `tests/lambda_otel.rs`: a task's span is exported in the same flush as its invocation.
- `tests/mcp.rs`: a tool's task lands in the per-invocation scope.
- `cargo check -p lesto --no-default-features --features lambda` compiles the extension.

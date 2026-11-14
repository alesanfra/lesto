# 17. Background tasks: work after the response

Some work does not need to finish before the caller gets an answer: a confirmation email, a
cache warm-up, an audit line. `BackgroundTasks` starts it so it runs after the response has been
sent. It is FastAPI's `BackgroundTasks`, and what Vercel and Cloudflare call `waitUntil`. The
code of this chapter is in `examples/99-tutorial/src/background.rs`.

## Best effort, not durable

Read this first. A task lives in the memory of **one process**. A crash, an out-of-memory kill,
a deploy that outlasts the shutdown deadline or a Lambda timeout loses it, and nothing retries
it. Work that must happen (charge a card, record an order, notify a partner who will not ask
twice) belongs in a queue (SQS, a job table, a database outbox), written in the request's own
transaction. Use tasks for what is nice to have done soon and harmless to lose.

What tasks do not give you, on purpose:

- **Durability and retries.** See above.
- **Results.** A task returns nothing to the request: it is over by the time the task runs.
  There is no handle to await or cancel.
- **Scheduling.** No delays, cron, priorities or concurrency limits.
- **`wasm32`.** `BackgroundTasks` is not available on Cloudflare Workers.

## The extractor

```rust
use lesto::prelude::*;

/// Create an order; the confirmation is sent after the response.
#[lesto::post("/orders", status = 201)]
async fn create_order(
    tasks: BackgroundTasks,
    State(mailer): State<Mailer>,
    Json(order): Json<Order>,
) -> Json<Created> {
    let created = Created { id: 1 };
    tasks.spawn(async move {
        // Runs after the response; an Err is logged, the customer never sees it.
        mailer.send(&order.item).await
    });
    Json(created)
}
```

`BackgroundTasks` is a handler argument like any other. It adds nothing to the OpenAPI document,
needs no feature and no setup on the `App`, and can sit in any position (it is not a body
extractor). `spawn` takes a future, starts it on the runtime and returns at once; the task
outlives the request, so it must own what it uses (`async move`, `'static`, `Send`).

A task returns `()` or a `Result<(), E>` for any `E: Display`. Many tasks per request are fine:
they run concurrently, in no particular order. When the order matters, write one task with
sequential `.await`s.

`mailer.send` above is any async call that returns `Result<(), MailError>`; in the example it
sleeps for 50 ms and records the recipient.

## Errors and panics

A task that returns `Err(e)` is logged at `ERROR`, target `lesto::background`, with `e` as the
`error` field. A task that panics is caught, logged at `ERROR` with the panic message, and never
propagated. Either way the response was sent already and does not change:

```rust
/// A task that returns `()` and one that panics: neither reaches the caller.
#[lesto::post("/audit", status = 202)]
async fn audit(tasks: BackgroundTasks) -> &'static str {
    tasks.spawn(async move {
        lesto::tracing::info!("audit line written");
    });
    tasks.spawn(async move {
        let _: u32 = "not a number".parse().unwrap(); // a bug: panics
    });
    "accepted"
}
```

Errors are your own type, so lesto asks only for `Display`. If a failure needs more than a log
line (a retry, an alert), the task is the place to do it: it has no caller to report to.

## Tracing

Each task runs in a span named `background {name}`, `otel.kind = "internal"`, a child of the span
current when `spawn` was called, which in a handler is the request span. The request span closes
first; backends draw the task as work after the response, which is what happened. `{name}` is the
function the `async` block was written in (`background create_order`); a task that is not an
`async` block of a function is just `background`. Logs the task writes carry its span, so with
`otel` (chapter 15) they line up with the request's trace.

## On a server

`App::serve` keeps one list of running tasks for the process. After the server stops (SIGTERM or
Ctrl-C, and every in-flight request finished) it waits for the tasks, inside the **same**
`App::shutdown_timeout` that already bounds the requests: 30 seconds by default. An
orchestrator's grace period does not grow because tasks exist. Tasks still running at the
deadline are abandoned, and lesto logs a warning with how many. With `shutdown_timeout(None)` it
waits without a bound.

Between shutdowns nothing limits the tasks: they are ordinary Tokio tasks. A burst of requests
that each start a slow task starts that many slow tasks. If that is a risk, cap the work yourself
(a `Semaphore` in the state), or use a queue.

## Testing

Through `oneshot` the tasks run on the test's runtime, after the response. A test that asserts
an effect has to wait for it, and the simplest way is a channel the task writes to:

```rust
#[lesto::test]
async fn the_task_runs_after_the_response() {
    let (tx, mut sent) = mpsc::unbounded_channel();
    let mailer = Mailer {
        sent: tx,
        failing: false,
    };
    let router = orders_app(mailer).into_router();

    let body = json!({"item": "book"}).to_string();
    let response = router.oneshot(post("/orders", body)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    // The response is out; the mail is not sent yet (the task sleeps first).
    assert!(sent.try_recv().is_err());

    let to = lesto::tokio::time::timeout(Duration::from_secs(5), sent.recv())
        .await
        .expect("the task never ran")
        .unwrap();
    assert_eq!(to, "book");
}
```

Always put a timeout on the wait, so a task that never runs fails the test instead of hanging
it. Through `lesto::lambda::test::invoke` (chapter 14) no channel is needed: it waits for the
tasks before returning, so the effects can be asserted right after the call.

## In Lambda

Lambda freezes the execution environment as soon as the response is posted, and a frozen
environment runs nothing. lesto therefore holds the freeze until the tasks of the invocation are
done, through an internal extension, so a task started in a handler runs to completion on its
own invocation. That has a cost in time, in money and in capacity, and a deadline: all in
[Tasks after the response](14-aws-lambda.md#tasks-after-the-response) of chapter 14. Tasks that
must not depend on any of it go to a queue.

## When to use a queue instead

Use a queue (or an outbox) when the work:

- must happen even if the process dies, which is every case where losing it is a bug rather than
  an inconvenience;
- may take longer than the shutdown grace period, or in Lambda longer than the time left on the
  invocation;
- needs retries with backoff, a dead-letter store, or a rate limit;
- should be done by a different process, sized and scaled separately.

Tasks suit the opposite: short, idempotent or harmless to lose, and tied to the request that
caused them.

## Recap

- `BackgroundTasks` as a handler argument; `tasks.spawn(async move { .. })` returns at once, the
  task runs after the response. It is best effort: a crash or a timeout loses it.
- A task returns `()` or `Result<(), E: Display>`; an `Err` or a panic is logged at `ERROR`
  (target `lesto::background`) and never reaches the caller.
- Span `background {name}`, a child of the request span.
- On a server, shutdown waits for the tasks inside `shutdown_timeout`, then abandons the rest
  with a warning.
- In tests, wait on a channel the task writes to; `lambda::test::invoke` waits for you.
- In Lambda the freeze waits for the tasks, within the deadline and at the price of billed time
  and capacity (chapter 14).
- Anything that must happen goes to a queue.

Appendices: [From FastAPI to lesto](A-from-fastapi-to-lesto.md), [Common problems](B-common-problems.md),
[Why axum and not actix-web](C-why-axum.md).

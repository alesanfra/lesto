# 11. Middleware and axum

lesto is a thin layer over [axum](https://docs.rs/axum). Everything that works with axum, in
particular the middleware of the **tower** ecosystem, works here too.

## Adding a layer

```toml
tower-http = { version = "0.6", features = ["cors", "trace", "timeout", "compression-gzip"] }
tracing-subscriber = "0.3"
```

```rust
use std::time::Duration;
use lesto::prelude::*;
use tower_http::cors::CorsLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt().init();

    App::new()
        .routes(routes![root])
        .layer(TraceLayer::new_for_http())               // log every request
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .layer(CorsLayer::permissive())                  // in production: explicit allow_origin
        .serve()
        .await
}
```

`layer` applies to the routes registered **before** the call, as in axum. Put it after your
`routes` / `nest` calls.

## Middleware written by you

`axum::middleware::from_fn` turns a function into a middleware:

```rust
use lesto::axum::extract::Request;
use lesto::axum::middleware::{self, Next};
use lesto::axum::response::Response;

async fn add_request_id(req: Request, next: Next) -> Response {
    let id = uuid::Uuid::new_v4().to_string();
    let mut res = next.run(req).await;
    res.headers_mut().insert("x-request-id", id.parse().unwrap());
    res
}

App::new()
    .routes(routes![root])
    .layer(middleware::from_fn(add_request_id))
```

A middleware can also reject the request by returning an `HttpError`:

```rust
async fn require_json(req: Request, next: Next) -> Result<Response, HttpError> {
    if req.method() == lesto::http::Method::POST
        && !req.headers().get("content-type").is_some_and(|v| v.as_bytes().starts_with(b"application/json"))
    {
        return Err(HttpError::new(415, "JSON only"));
    }
    Ok(next.run(req).await)
}
```

The response will be a problem+json like every other error.

## Dropping down to axum

When you need something lesto does not expose:

```rust
// Modify the underlying Router (routes added this way are not documented)
App::new()
    .routes(routes![root])
    .map_router(|router| router.route("/legacy", lesto::axum::routing::get(legacy_handler)))

// Or take the Router and carry on with axum
let router: lesto::axum::Router = App::new().routes(routes![root]).into_router();
let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
lesto::axum::serve(listener, router).await?;
```

`into_router` mounts the documentation pages, the problem+json fallbacks (`404`, `405`), the
panic catcher (`500`) and the error middleware, then gives you back an ordinary `axum::Router`:
you can pass it to `axum::serve`, merge it with other routers, or serve it over TLS with
`axum-server`.

### Keeping lesto's behavior on a plain router

The three layers `into_router` installs are public, so a router that is not built by lesto at
all can have them:

```rust
use lesto::layers::{CatchPanicLayer, ProblemLayer, RequestSpanLayer};

let router = lesto::axum::Router::new()
    .route("/legacy", lesto::axum::routing::get(legacy_handler))
    .layer(CatchPanicLayer)
    .layer(ProblemLayer)
    .layer(RequestSpanLayer::new());
```

- `ProblemLayer` gives every RFC 9457 response its `instance` (the request path).
- `CatchPanicLayer` turns a panic into a `500` problem.
- `RequestSpanLayer` opens the request span of chapter 15. `RequestSpanLayer::with(Trace::off())`
  is the same layer with the span switched off.

The order above is the one lesto uses: the last layer added is the outermost, so the span sees
the status the client sees, and the panic `500` still gets its `instance`. Inside `into_router`
the three are stacked and added in a single `.layer(...)` call, because each call makes axum
re-box every route.

## Shutdown and panics

`App::serve` (and `serve_at`, `serve_on`) already stops gracefully on `SIGTERM` or `Ctrl-C`:
the listener closes, in-flight requests finish, then the future returns. Kubernetes, systemd and
`lesto dev` all send `SIGTERM` first, so no extra code is needed.

Waiting has a limit: 30 seconds by default. A handler still running then (a stuck upstream call,
a request that never ends) no longer keeps the process alive until the orchestrator sends
`SIGKILL`; the serve future returns, a `warn` log says so, and `main` exits. Change the limit
with `.shutdown_timeout(Duration::from_secs(10))`, keeping it below your platform's grace period
(Kubernetes' `terminationGracePeriodSeconds` is 30 s), or wait forever with
`.shutdown_timeout(None)`.

To stop on something else, use `serve_until` with your own future:

```rust
let listener = lesto::listener(("0.0.0.0", 8000)).await?;
App::new()
    .routes(routes![root])
    .serve_until(listener, async { shutdown_rx.await.ok(); })
    .await?;
```

If you serve the router with `axum::serve` yourself, add `.with_graceful_shutdown(lesto::shutdown_signal())`.

A handler that panics answers `500` as problem+json instead of dropping the connection; the
panic message is printed by the panic hook and recorded with `tracing::error!`, never sent to
the client.

## Logging and request ids

lesto emits `tracing` events for what it handles itself (`500`s, panics, failed
serializations) and nothing per request: pick the access log you want. `TraceLayer` above logs
every request; `tower_http::request_id::{SetRequestIdLayer, PropagateRequestIdLayer}` add and
echo an `x-request-id`, and `tracing_subscriber` with the `json` feature writes one JSON line
per event for your log collector.

## Using axum's extractors

All axum extractors can be used in handlers. Those that have nothing to do with the client
(`HeaderMap`, `Method`, `Uri`, `Request`, `Bytes`, `String`, `State`, `Extension`) do not appear
in the documentation. `axum::Json<T>`, `axum::extract::Query<T>` and `axum::extract::Path<T>` are
documented like lesto's but **without garde validation**: useful when `T` is an external type you
cannot derive `Validate` on.

For an axum or third-party extractor lesto does not know, implement `OperationInput` on a newtype
(chapter 8).

## Recap

- `.layer(...)` for tower middleware, after registering the routes.
- Graceful shutdown and panic-to-500 are built in; `serve_until` for a custom shutdown trigger.
- `middleware::from_fn` for ad hoc middleware; they can return `HttpError`.
- `map_router` and `into_router` for everything that is pure axum.
- `lesto::layers` if you leave lesto but want the problems, the panic catcher or the span.

Next: [Testing](12-testing.md).

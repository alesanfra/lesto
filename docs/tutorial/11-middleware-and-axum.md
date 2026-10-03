# 11. Middleware and axum

lesto is a thin layer over [axum](https://docs.rs/axum). Everything that works with axum, in
particular the middleware of the **tower** ecosystem, works here too.

## Adding a layer

The most common layers are built in (next section). Anything else from the tower ecosystem goes
through `layer`, here tower-http's `SetResponseHeaderLayer`:

```toml
tower-http = { version = "0.6", features = ["set-header"] }
```

```rust
use lesto::http::{HeaderValue, header};
use lesto::prelude::*;
use tower_http::set_header::SetResponseHeaderLayer;

#[lesto::main]
async fn main() -> std::io::Result<()> {
    App::new()
        .routes(routes![root])
        .layer(SetResponseHeaderLayer::if_not_present(   // unless the handler set one
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .serve()
        .await
}
```

`layer` applies to the routes registered **before** the call, as in axum. Put it after your
`routes` / `nest` calls.

## Built-in layers

What almost every service needs is one builder call, with errors answered as problems like the
rest:

```rust
use std::time::Duration;
use lesto::cors::{Any, CorsLayer};

App::new()
    .routes(routes![root])
    .timeout(Duration::from_secs(10))
    .body_limit(64 * 1024)
    .cors(CorsLayer::new().allow_origin(["https://app.example".parse()?]).allow_headers(Any))
    .compression()
    .request_id()
```

- `.timeout(Duration::from_secs(10))`: a request still running after ten seconds answers
  `503 Service Unavailable` (problem+json, with `instance`), and its handler is dropped. Off by
  default. The clock includes reading the body, so a client that trickles bytes in is cut off
  as well. `lesto::layers::TimeoutLayer` is the same thing for a plain axum router.
- `.body_limit(64 * 1024)`: a larger body answers `413 Payload Too Large`. Without the call the
  limit is axum's, **2 MB**; `.body_limit(None)` removes it (for uploads behind a proxy that
  limits them already).

- `.cors(layer)`: tower-http's `CorsLayer` (re-exported as `lesto::cors`), installed outside
  everything else, so a `404` or `500` problem carries the CORS headers too and the browser can
  show it. `CorsLayer::permissive()` is fine in development; in production name the origins.
- `.compression()`: gzip for clients that send `Accept-Encoding: gzip` (feature `compression`,
  on by default).
- `.request_id()`: every request gets an `x-request-id` (a UUID v4, or the one the client or a
  proxy sent), echoed on the response.

None of them costs anything when it is not set. Unlike `layer`, their position in the builder
chain does not matter.

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

## Coming from axum

An existing axum application does not have to be rewritten to start using lesto. Mount its
router as it is and move handlers over one at a time:

```rust
let legacy: lesto::axum::Router<AppState> = old_routes();   // your axum code, unchanged

App::new()
    .routes(routes![create_user, get_user])                 // the handlers moved so far
    .merge(legacy)                                          // everything else, as before
    .nest_router("/admin", admin_routes())                  // or under a prefix
    .with_state(state)
    .serve()
    .await
```

`merge` and `nest_router` take a plain `axum::Router` with the same state type. Its routes are
served but **not documented**: lesto documents a handler from its argument and return types,
and it never sees those of a finished router. They do get the rest: problem+json rendering, the
panic catcher, the request span, the timeout. `App::from(router)` starts an app from a router.

## Dropping down to axum

When you need something lesto does not expose:

```rust
// Modify the underlying Router (routes added this way are not documented)
App::new()
    .routes(routes![root])
    .map_router(|router| router.route("/legacy", lesto::axum::routing::get(legacy_handler)))

// Or take the Router and carry on with axum
let router: lesto::axum::Router = App::new().routes(routes![root]).into_router();
let listener = lesto::tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
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

## Logging

`App::serve` prints on stdout with nothing to set up: a line when it starts listening, one per
request (the access log), and every `tracing` event at `info` and above — yours and the ones
lesto writes for what it handles itself (`500`s, panics, timeouts, failed transactions):

```text
2026-10-03T09:15:00.120456Z  INFO lesto::app: listening on http://127.0.0.1:8000
2026-10-03T09:15:01.004311Z  INFO POST /notes 201 8.9ms
2026-10-03T09:15:01.200118Z  WARN GET /notes/7 notes::handlers: slow lookup user=7
2026-10-03T09:15:01.200540Z  INFO GET /notes/7 200 412µs
2026-10-03T09:15:02.381090Z ERROR GET /boom lesto::layers: handler panicked panic=boom
2026-10-03T09:15:02.381233Z ERROR GET /boom 500 71µs
```

An event that happens during a request starts with the request's method and path, and inside
a store transaction (chapter 13) with the store method too. A log collector wants JSON instead:
`LESTO_LOG=json` writes one flat object per line, the request's fields next to the event's.

```json
{"timestamp":"2026-10-03T09:15:01.200118Z","level":"WARN","target":"notes::handlers","message":"slow lookup","http.request.method":"GET","http.route":"/notes/{id}","url.path":"/notes/7","user":7}
{"timestamp":"2026-10-03T09:15:01.200540Z","level":"INFO","target":"lesto::access","message":"GET /notes/7 200","http.request.method":"GET","http.route":"/notes/{id}","url.path":"/notes/7","http.response.status_code":200,"duration_ms":0.412}
```

| variable | meaning |
|---|---|
| `LESTO_LOG` | `text` (the default), `json` or `off` |
| `RUST_LOG` | what is printed, `info` by default: `RUST_LOG=info,lesto=debug`, `RUST_LOG=warn` |
| `NO_COLOR` | no colors, even on a terminal (there are none when stdout is not one) |

The access log is an event with the target `lesto::access`, at `INFO` (`ERROR` for a `5xx`), so
`RUST_LOG=info,lesto::access=off` turns it off and keeps the rest. It shows the path, never the
query string, which is application data.

**Your own subscriber wins.** Install one before `serve` (a file, another format, a filter
reloaded at runtime) and lesto installs nothing. `lesto::log::Console` is the layer lesto
installs, so a subscriber of your own can keep its format. The `log` feature is on by default;
`default-features = false` leaves it out, and with it `tracing-subscriber`. Where `serve` is
not the entry point (a worker, a command-line tool), `lesto::log::init()` does the same thing.

`.request_id()` adds and echoes an `x-request-id`; chapter 15 sends the same events, with their
trace, to an OpenTelemetry backend.

## Using axum's extractors

All axum extractors can be used in handlers. Those that have nothing to do with the client
(`HeaderMap`, `Method`, `Uri`, `Request`, `Bytes`, `String`, `State`, `Extension`) do not appear
in the documentation. `axum::Json<T>`, `axum::extract::Query<T>` and `axum::extract::Path<T>` are
documented like lesto's but **without garde validation**: useful when `T` is an external type you
cannot derive `Validate` on. When `T` *does* implement `Validate`, the rules would be silently
skipped, so lesto warns on the argument ("`axum::Json<T>` ... does not run `T`'s garde rules");
the warning is a deprecation, so `#[allow(deprecated)]` on the handler keeps an intentional one
quiet.

For an axum or third-party extractor lesto does not know, implement `OperationInput` on a newtype
(chapter 8).

## Recap

- `.layer(...)` for tower middleware, after registering the routes.
- Graceful shutdown and panic-to-500 are built in; `serve_until` for a custom shutdown trigger.
- Logs on stdout out of the box, one line per request: `LESTO_LOG=json` for a collector,
  `RUST_LOG` to filter, a subscriber of your own to replace them.
- `middleware::from_fn` for ad hoc middleware; they can return `HttpError`.
- `map_router` and `into_router` for everything that is pure axum.
- `lesto::layers` if you leave lesto but want the problems, the panic catcher or the span.

Next: [Testing](12-testing.md).

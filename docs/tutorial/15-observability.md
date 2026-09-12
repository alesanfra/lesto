# 15. Observability: tracing and OpenTelemetry

Every request a lesto router handles runs inside a `tracing` span whose fields are the
[OpenTelemetry semantic conventions for HTTP servers](https://opentelemetry.io/docs/specs/semconv/http/http-spans/),
and every store transaction (chapter 13) inside a client span with the
[database conventions](https://opentelemetry.io/docs/specs/semconv/database/database-spans/).
Nothing to add to a handler. With the `otel` feature, nothing to add to `main` either: the
`OTEL_*` environment variables decide where the spans — and the log events — go.

The runnable version of this chapter is `examples/04-openobserve`: an API plus a local
[OpenObserve](https://openobserve.ai/) in Docker, where you can watch the traces and the logs
arrive.

## Sending traces and logs, in two lines of configuration

```toml
[dependencies]
lesto = { path = "../lesto/crates/lesto", features = ["otel"] }
```

```rust
#[tokio::main]
async fn main() -> std::io::Result<()> {
    build_app().serve().await          // unchanged
}
```

```sh
export OTEL_SERVICE_NAME=notes
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
cargo run
```

`App::serve` sees the endpoint, installs a subscriber that prints to the console **and** exports
over OTLP — spans as traces, `tracing` events as log records — and flushes what is buffered when
the server shuts down. Without the endpoint nothing is exported, which is what you want in tests
and under `lesto dev`.

| variable | meaning |
|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | where to send telemetry; `/v1/traces` and `/v1/logs` are appended. Unset: no export |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `..._LOGS_ENDPOINT` | the same per signal, used exactly as given |
| `OTEL_EXPORTER_OTLP_HEADERS` | `key1=value1,key2=value2`, for a backend that wants an `Authorization` |
| `OTEL_SERVICE_NAME` | `service.name`; defaults to the title of your OpenAPI document |
| `OTEL_TRACES_EXPORTER=none` | keep the logs, stop exporting spans |
| `OTEL_LOGS_EXPORTER=none` | keep the spans, stop exporting logs |
| `OTEL_SDK_DISABLED=true` | keep the console, stop both |
| `RUST_LOG` | console and export filter, `info` by default |

## Logs, next to the trace they belong to

Every `tracing` event becomes an OTLP log record carrying the trace and span it happened in, so
a backend shows the logs of a request under its trace and `RUST_LOG` filters both:

```rust
tracing::info!(author = %store.principal().name, "creating a note");
```

```
body      "creating a note"
severity  INFO
trace_id  c4278fbb…      span_id  b1a587ce…      author "ada"
```

Errors lesto logs itself (`store method failed`, `handler panicked`) arrive the same way, with
the cause as an attribute — the response body still says nothing about it.

The telemetry stack is kept out of its own pipeline: records from `opentelemetry*`, `reqwest`,
`hyper` and friends are printed on the console but never exported, or an export failure would be
logged, exported, and fail again.

The export is OTLP over HTTP/protobuf. Two things lesto deliberately does not do: it never
touches an application that has already installed a `tracing` subscriber, and it never exports
without an endpoint. So a pipeline of your own (gRPC, a custom sampler, spans *and* logs) stays
possible: build your subscriber before `serve`, and lesto stands aside.

Where `serve` is not the entry point — `lesto::lambda::serve`, a worker, a test — call it
yourself and keep the guard alive:

```rust
let telemetry = lesto::otel::init_named("notes");
// ... run ...
telemetry.shutdown();          // or let it drop
```

## What a request span contains

```
http.server.request
  otel.name                 "GET /users/{id}"       the span name a collector shows
  otel.kind                 "server"
  otel.status_code          "ERROR" on 5xx, unset otherwise
  http.request.method       "GET" (or "_OTHER" plus http.request.method_original)
  http.route                "/users/{id}"           the route, never the concrete path
  http.response.status_code 200
  url.path                  "/users/7"
  url.scheme                "http"
  server.address/port       from X-Forwarded-Host, :authority or Host
  client.address            the peer, or X-Forwarded-For when trusted
  network.peer.address/port, network.protocol.version, user_agent.original
  error.type                "500" when the server failed
```

The name is `{method} {route}`, which keeps the cardinality of a trace backend sane: `/users/7`
and `/users/8` are the same operation. A request that matched no route has no `http.route`, and
its name is the method alone.

A `4xx` is the client's fault, so the span status stays unset; a `5xx` sets `error.type` and
`otel.status_code`. Panics answer `500`, so they show up the same way.

## Traces that span services

With the `otel` feature the `traceparent` of an incoming request becomes the parent of the
request span, so a call from another service continues the same trace:

```sh
curl localhost:8000/notes \
  -H 'traceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01'
```

The span lands in trace `4bf92f35…` with `00f067aa0ba902b7` as its parent. lesto installs the
W3C propagator; for B3 or another format, set yours with
`opentelemetry::global::set_text_map_propagator` after `init`.

## Store spans

With the `db` feature, `read`, `write`, `read_with` and `write_with` open a client span around
the transaction, as a child of the request span:

```
GET /notes
└── BEGIN DEFERRED
      otel.kind            "client"
      db.system.name       "sqlite"
      db.operation.name    "BEGIN DEFERRED"    the statement the transaction opens with
      error.type           on failure: "23505", "1", "Forbidden", ...
      db.response.status_code  the database's own code
```

The span starts at the `BEGIN` and ends at the commit or the rollback, so its duration is the
time the request really spent holding a transaction — including the retries `read_with` and
`write_with` make, which appear as `transaction conflicted, retrying` events inside it.

A requirement that fails has no span at all: a `403` never reaches the database.

lesto records no `db.query.text`. The statement it builds carries the principal's
`transaction_settings` (an identity), and the queries themselves live in your closure, which
lesto never sees. Per-statement spans are sqlx's business.

## Configuring what is recorded

```rust
use lesto::Trace;

App::new()
    .routes(routes![/* .. */])
    .trace(Trace::new().query(true).forwarded(true))
```

- `query(true)` records `url.query`. Off by default because a query string is application data:
  tokens, emails, filters. The values of the keys the conventions call out (`sig`,
  `X-Amz-Signature`, `X-Amz-Credential`, `X-Amz-Security-Token`, `X-Goog-Signature`) are replaced
  by `REDACTED` even when it is on.
- `forwarded(true)` trusts `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Forwarded-Host` for
  `client.address`, `url.scheme` and `server.address`. Turn it on when a proxy you control
  rewrites those headers — anybody can send them.
- `Trace::off()` removes the span entirely.

`client.address` without a proxy comes from the peer address, which axum only knows when the app
is served with `into_make_service_with_connect_info`; `App::serve` does not, so behind a proxy
`forwarded(true)` is the useful setting.

## Your own spans and events

Inside a handler `tracing` works as usual, and everything lands under the request span:

```rust
#[lesto::post("/notes")]
async fn create_note(store: NoteStore<ReadWrite, User>, Json(body): Json<NoteCreate>)
    -> Result<Json<Note>, lesto::db::Error>
{
    tracing::info!(author = %store.principal().name, "creating a note");
    Ok(Json(store.create(body).await?))
}
```

Events become span events and fields become attributes, so `tracing::info!` is enough — no
OpenTelemetry API in your code. To time a helper, put `#[tracing::instrument]` on it.

## In Lambda

The spans are the same, and `traceparent` from API Gateway is picked up as usual, but
`lesto::lambda::serve` sets nothing up: a Lambda instance is frozen between invocations, so a
batching exporter loses spans. Call `lesto::otel::init()` in `main` and flush before returning,
or send to the OpenTelemetry Lambda layer, which does that for you.

## Recap

- Request spans are on by default, named `{method} {http.route}`, with the HTTP semantic
  conventions as fields; `5xx` marks the span as an error, `4xx` does not.
- Store transactions get a client span with the database conventions, as a child of the request.
- The `otel` feature plus `OTEL_EXPORTER_OTLP_ENDPOINT` is the whole setup: `App::serve` installs
  the console subscriber and the OTLP export of spans and logs, and flushes on shutdown.
- Log records carry their `trace_id` and `span_id`, so logs and traces line up in the backend.
- An incoming `traceparent` continues the trace here.
- `App::trace(Trace::new()...)` opts `url.query` in, trusts forwarding headers, or turns the
  span off.
- `examples/04-openobserve` runs the whole thing against a local OpenObserve.

Appendices: [From FastAPI to lesto](A-from-fastapi-to-lesto.md), [Common problems](B-common-problems.md),
[Why axum and not actix-web](C-why-axum.md).

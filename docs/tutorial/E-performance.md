# E. Performance

## Per request

What lesto costs over the `axum::Router` it builds on, per request, measured through
`tower::Service` calls with no networking
([`benches/overhead.rs`](https://github.com/alesanfra/lesto/blob/main/crates/lesto/benches/overhead.rs),
`cargo bench -p lesto`; an Apple laptop, best of five rounds of 200,000 requests):

| Request | axum 0.8 | lesto |
|---|---:|---:|
| `GET /hello` | 616 ns | 911 ns |
| `POST /items`, JSON body validated, `status = 201` | 1,187 ns | 1,498 ns |
| `GET /missing` → `404` problem+json | 388 ns | 1,260 ns |
| `GET` a handler returning `HttpError::not_found` | 805 ns | 1,215 ns |

The difference is what lesto adds: the request span, the panic catcher, and an RFC 9457 body
where axum answers with none. A `422` costs more again (about 3.0 µs) because it also runs
garde and reports every failed check with its location — plain axum does not validate at all,
so there is nothing to compare it with.

`Trace::off()` is not on that list on purpose: it saves about 13 ns, because it is a branch
inside a layer that is installed anyway. Use it when you do not want a request span, not when
you want speed.
[`scripts/bench-http.sh`](https://github.com/alesanfra/lesto/blob/main/scripts/bench-http.sh)
runs the same application behind `oha` over a real socket, where the numbers above are lost in
the noise of the network.

## Build time

Build time is the other cost, and the reason `otel` is not a default feature. `examples/02-notes`
(`sqlite`), measured 2026-09-23 on an Apple Silicon laptop with the workspace's dev profile
(dependencies at `opt-level = 3`):

| | crates | clean build | rebuild after editing the app |
|---|---|---|---|
| without `otel` | 177 | 94 s | 1.0 s |
| with `otel` | 210 | 118 s | 1.1 s |

The OpenTelemetry SDK, the OTLP exporter and its HTTP client add 33 crates and about a quarter
to a clean build; a rebuild of your own code does not notice. Turn it on where telemetry is
exported, typically in the deployed build.

# 04 — OpenTelemetry: traces and logs

An unauthenticated API with no telemetry code at all: `lesto` is built with the `otel` feature,
the environment says where the collector is, and `App::serve` sets everything up — console logs,
OTLP export, a flush on shutdown. `main` only builds the app and calls `serve()`.

`compose.yaml` has two backends to send it to. Pick one:

| | [Jaeger](https://www.jaegertracing.io/) | [OpenObserve](https://openobserve.ai/) |
|---|---|---|
| signals | traces | traces **and** logs |
| setup | none: open the UI | log in, demo credentials |
| OTLP endpoint | `http://localhost:4318` | `http://localhost:5080/api/default` |
| authentication | none | `Authorization: Basic …` |
| UI | <http://localhost:16686> | <http://localhost:5080> |

Both can run at the same time; their ports do not overlap.

## Option A — Jaeger (traces, zero configuration)

```sh
cd examples/04-opentelemetry
docker compose up -d jaeger

export OTEL_SERVICE_NAME=lesto-demo
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
export OTEL_LOGS_EXPORTER=none          # Jaeger ingests traces only
export LESTO_PORT=8000

cargo run -p opentelemetry-example
```

`OTEL_LOGS_EXPORTER=none` matters: Jaeger answers `404` on `/v1/logs`, so without it every batch
of log records fails. Traces are at <http://localhost:16686>, service `lesto-demo`.

## Option B — OpenObserve (traces and logs)

```sh
cd examples/04-opentelemetry
docker compose up -d openobserve

export OTEL_SERVICE_NAME=lesto-demo
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:5080/api/default
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic cm9vdEBleGFtcGxlLmNvbTpDb21wbGV4cGFzcyMxMjM="
export LESTO_PORT=8000

cargo run -p opentelemetry-example
```

The `Authorization` value is `base64("root@example.com:Complexpass#123")`, the credentials in
`compose.yaml`; with others, rebuild it with `printf 'email:password' | base64`. `default` in the
endpoint is the organization, and the exporters append `/v1/traces` and `/v1/logs` themselves.
The UI is at <http://localhost:5080>, same credentials.

`./verify.sh` checks this option from the command line: it sends a few requests and asks
OpenObserve for the spans and the log records it received.

## Make some telemetry

```sh
curl localhost:8000/hello
curl -X POST localhost:8000/notes -H 'content-type: application/json' -d '{"text":"hello"}'
curl localhost:8000/notes
curl -i localhost:8000/boom      # 500: the span is marked as an error
curl -i localhost:8000/broken    # 500 from the database: the store span carries the code
curl -i localhost:8000/notes -X POST -H 'content-type: application/json' -d '{"text":""}'   # 422
```

Continue a trace started elsewhere, the way another service would:

```sh
curl localhost:8000/notes \
  -H 'traceparent: 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01'
```

The request span becomes a child of that trace instead of starting a new one — that is what the
`otel` feature adds on top of the spans themselves.

## What you should see

A `GET /notes` trace has two spans:

```
GET /notes                    http.route=/notes, http.response.status_code=200
└── NoteStore::list           db.system.name=sqlite
```

The child span is named after the store method that opened the transaction. `GET /boom` has
`error.type=500` on the request span, and `GET /broken` has the SQLite error code (`1`) on its
store span, named `opentelemetry_example::broken` because that closure lives in the handler
itself.

With OpenObserve, **logs** are in the `default` stream under **Logs**, one record per `tracing`
event, each carrying the `trace_id` and `span_id` of the request it happened in:

| body | severity | trace |
|---|---|---|
| `creating a note` | INFO | the `POST /notes` trace |
| `store method failed` | ERROR | the `GET /broken` trace, with `exception_message` |

## Stop

```sh
docker compose down -v
```

## Notes

- Nothing is exported while `OTEL_EXPORTER_OTLP_ENDPOINT` is unset: without it the application
  only logs to the console, which is what you want in tests and in `lesto dev`.
- `OTEL_SDK_DISABLED=true` keeps the console and turns both exports off; `OTEL_TRACES_EXPORTER=none`
  and `OTEL_LOGS_EXPORTER=none` turn off one signal each.
- `RUST_LOG` filters the console and the exported logs (`RUST_LOG=warn,lesto=info`); spans are
  emitted at `INFO`. `RUST_LOG=sqlx::query=debug` adds each SQL statement, with its timing,
  inside the span of the store method that ran it.
- The export is OTLP over HTTP/protobuf. For gRPC, or for a subscriber of your own, build the
  pipeline by hand — lesto stands aside as soon as a subscriber is installed (tutorial,
  chapter 15).

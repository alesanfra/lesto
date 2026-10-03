# Examples

Each folder is a package of the workspace, run from the repository root with `cargo run -p
<package>` (or `lesto dev -p <package>`, which rebuilds and restarts on every save). They depend
on the `lesto` of this checkout, not on the published crate.

| folder | package | what it shows | tutorial |
|---|---|---|---|
| [`01-hello`](01-hello) | `hello` | the smallest app: one route, docs at `/docs` | [1](../docs/tutorial/01-first-steps.md) |
| [`02-notes`](02-notes) | `notes` | full CRUD on SQLite with `lesto::db`: stores, principals, permissions, views, MCP tools, a resource and a prompt | [13](../docs/tutorial/13-databases.md), [16](../docs/tutorial/16-mcp.md) |
| [`03-lambda`](03-lambda) | `lambda` | one binary that serves locally and on AWS Lambda | [14](../docs/tutorial/14-aws-lambda.md) |
| [`04-opentelemetry`](04-opentelemetry) | `opentelemetry-example` | traces, logs and metrics to Jaeger or OpenObserve, configured by `OTEL_*` only | [15](../docs/tutorial/15-observability.md) |
| [`05-routers`](05-routers) | `routers` | two APIs in one service, mounted with `nest` like FastAPI routers | [10](../docs/tutorial/10-bigger-projects.md) |
| [`06-oidc`](06-oidc) | `oidc-example` | bearer JWTs from a real OpenID Connect provider | [9](../docs/tutorial/09-security.md) |
| [`99-tutorial`](99-tutorial) | `tutorial` | every snippet of the tutorial, compiled and tested | all |

Port 8000 (the default) is often taken on a development machine, hence `LESTO_PORT=8765` below.
Every example prints a line per request on stdout; `LESTO_LOG=json` switches to JSON,
`LESTO_LOG=off` silences it (chapter 11).

## Running them

```sh
# 01: one route
LESTO_PORT=8765 cargo run -p hello
curl http://127.0.0.1:8765/hello

# 02: notes on SQLite (in memory: every start is empty)
LESTO_PORT=8765 cargo run -p notes
curl -s http://127.0.0.1:8765/notes
curl -s -X POST http://127.0.0.1:8765/notes -H 'Authorization: Bearer bob-token' \
     -H 'Content-Type: application/json' -d '{"text":"hello"}'

# 03: locally like any app; `cargo lambda build --release --arm64 -p lambda` for Lambda
LESTO_PORT=8765 cargo run -p lambda

# 05: two APIs under /api/app/v1 and /api/analytics/v1
LESTO_PORT=8765 cargo run -p routers
curl http://127.0.0.1:8765/api/analytics/v1/sales
```

`04-opentelemetry` and `06-oidc` need Docker for their backend (a collector, an OpenID Connect
provider): their own `README.md` has the details. Each has a `start.sh`, which starts the
backend, waits for it and runs the example wired to it, and a `stop.sh`:

```sh
examples/04-opentelemetry/start.sh            # OpenObserve, UI at http://localhost:5080
examples/04-opentelemetry/start.sh jaeger     # Jaeger, UI at http://localhost:16686
examples/04-opentelemetry/stop.sh             # stop both; --clean also drops the data

examples/06-oidc/start.sh                     # mock-oauth2-server, then the example on :8765
examples/06-oidc/stop.sh
```

In `02-notes`, anyone can read; `bob-token` can write, `alice-token` can write and delete. The
same API is an MCP server at `/mcp`, every route but `DELETE`.

## Testing them

```sh
cargo test -p notes -p routers -p lambda -p tutorial   # in-process, no port, no Docker
bash examples/02-notes/verify.sh                       # 02 end to end with curl
```

`verify.sh` checks every route of `02-notes` with curl — CRUD, `401`/`403`/`404`/`422`,
problem+json errors, the docs pages and MCP — and prints `ok` or `FAIL` per check. It starts the
example on `$BASE` (default `http://127.0.0.1:8765`) when nothing answers there, and stops it at
the end; against a server you started yourself, that server must be fresh (the database is in
memory and the first check expects no notes). It needs bash and curl, nothing else.

`04-opentelemetry/verify.sh` and `06-oidc/verify.sh` do the same against their Docker backends
(see their READMEs); they are not run in CI.

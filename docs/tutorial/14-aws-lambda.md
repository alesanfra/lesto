# 14. Deploying to AWS Lambda

The `lambda` feature runs a lesto application as a Lambda function behind API Gateway, a Function URL
or an Application Load Balancer. The same binary keeps working locally, under `lesto dev`, and in
tests. The code of this chapter is `examples/03-lambda`.

## Setup

```toml
[dependencies]
lesto = { version = "0.1", features = ["lambda"] }
```

Change one line in `main`:

```rust
#[lesto::main]
async fn main() -> Result<(), lesto::lambda::Error> {
    lesto::lambda::serve(build_app().with_state(Notes::default())).await
}
```

`lesto::lambda::serve` looks at the environment: inside Lambda (the runtime sets
`AWS_LAMBDA_RUNTIME_API`) it starts the Lambda runtime and answers events; anywhere else it
behaves exactly like `App::serve` (`LESTO_HOST`/`LESTO_PORT`, socket handover from `lesto dev`
included). `serve_at(app, addr)` takes an explicit local address. No `cfg`, no second binary.

## What is different in Lambda

**Events, not sockets.** API Gateway turns each HTTP request into a JSON event and the runtime
turns it back into an HTTP request for your router. `lesto::lambda` accepts the three formats you
can meet: REST API (payload 1.0), HTTP API and Function URL (payload 2.0), ALB. Handlers,
extractors, validation, RFC 9457 errors and the documentation pages behave as they do locally.

**The stage prefix.** A REST API is deployed to a *stage* (`prod`, `v1`), and the stage is part
of the public URL: `https://abc.execute-api.eu-west-1.amazonaws.com/prod/notes`. By default
`lesto::lambda` removes the stage from the path before routing, so your routes stay `/notes` and
`instance` in error responses is `/notes/99`, not `/prod/notes/99`. If you want the stage to be
part of your routes, use `serve_with` and `Options::default().keep_stage(true)`. HTTP APIs with the
`$default` stage and Function URLs have no prefix.

**Documentation pages.** `/docs` and `/swagger` link `openapi.json` with a relative URL, so
`https://.../prod/docs` loads `https://.../prod/openapi.json` without configuration. If you set
`App::server("https://api.example.com/prod")`, the "Try it out" button uses that base URL.

**State.** A Lambda instance handles one request at a time and may be frozen or discarded at
any moment. Keep in-memory state (like the `Mutex<Vec<Note>>` of the example) for caches only;
real data goes to a database or another service. With `lesto::db`, build the pool once in
`main` and reuse it across invocations, as the example does with its state.

## Building and deploying

Lambda wants a Linux executable named `bootstrap`. The easiest way is
[cargo-lambda](https://www.cargo-lambda.info/):

```sh
cargo install cargo-lambda
cargo lambda build --release --arm64 -p lambda      # cross-compiles for Graviton
cargo lambda deploy lambda --enable-function-url    # creates the function and a Function URL
```

`cargo lambda deploy` prints the Function URL; `curl $URL/notes` answers, and `$URL/docs` shows
the API reference. For API Gateway create an HTTP API with a `$default` route pointing at the function
(payload format 2.0), or a REST API with an `ANY /{proxy+}` method and a stage. Infrastructure
tools (SAM, CDK, Terraform) work with the same `bootstrap` zip; `cargo lambda build --output-format zip`
produces it.

Environment variables (`DATABASE_URL`, tokens) are set on the function; `LESTO_HOST` and
`LESTO_PORT` are ignored in Lambda.

## Cold starts and binary size

A Rust Lambda cold-starts in tens of milliseconds, but a smaller binary loads faster. Build in
release mode, prefer `arm64`, and consider in `Cargo.toml`:

```toml
[profile.release]
lto = true
codegen-units = 1
strip = true
```

The `lambda` feature enables only the API Gateway and ALB event formats of the runtime; websockets are
not included.

## Testing with event fixtures

`lesto::lambda::test::invoke` feeds a raw event (the JSON API Gateway sends, which you can copy
from the Lambda console or from the AWS documentation) to your router and returns the HTTP
response, so you can test the Lambda path without deploying:

```rust
let router = build_app().with_state(Notes::default()).into_router();
let res = lesto::lambda::test::invoke(&router, &event("GET", "/notes/9", None)).await?;
assert_eq!(res.status(), StatusCode::NOT_FOUND);
assert_eq!(res.headers()["content-type"], "application/problem+json");
```

The response body is already collected (`Bytes`), ready for `serde_json::from_slice`.
`invoke_with(&router, event, Options::default().keep_stage(true))` tests the other stage setting; the
option belongs to the router you pass, so both settings can share a test file.

## Recap

- Replace `app.serve()` with `lesto::lambda::serve(app)`: Lambda when in Lambda, a plain server
  otherwise.
- REST API stages are stripped from the path by default (`Options::keep_stage` to keep them).
- Documentation pages work behind the stage because they link `openapi.json` relatively.
- Build with `cargo lambda build --release --arm64`, deploy with `cargo lambda deploy` or your
  infrastructure tool.
- Test the Lambda path in-process with `lesto::lambda::test::invoke` and event fixtures.

Next: [Observability: tracing and OpenTelemetry](15-observability.md).

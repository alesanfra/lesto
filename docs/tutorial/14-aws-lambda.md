# 14. Deploying to AWS Lambda

The `lambda` feature runs a lesto application as a Lambda function behind API Gateway, a Function URL
or an Application Load Balancer. The same binary keeps working locally, under `lesto dev`, and in
tests. The code of this chapter is `examples/03-lambda`.

## Setup

```toml
[dependencies]
lesto = { version = "0.3", features = ["lambda"] }
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

**Logs.** Lambda sends stdout to CloudWatch Logs, so the logs of chapter 11 need nothing: one
line per request and every `tracing` event. Set the function's log format to JSON and Lambda
sets `AWS_LAMBDA_LOG_FORMAT=JSON`, which makes lesto write JSON too (`LESTO_LOG` still wins), so
Logs Insights can query `http.route` or `http.response.status_code` directly.

**Traces.** With the `otel` feature, call `lesto::otel::init()` before `serve`: the spans and log
records of an invocation are exported right after its response, before Lambda freezes the
instance (a frozen instance exports nothing). Chapter 15 has the details,
`Options::flush_telemetry` to turn it off, `Options::xray` to join the X-Ray trace of the
invocation, and how to send the spans straight to X-Ray (lesto signs the request with the
function's role) without a layer; [Tracing with X-Ray, step by step](#tracing-with-x-ray-step-by-step)
below is the recipe.

**State.** A Lambda instance handles one request at a time and may be frozen or discarded at
any moment. Keep in-memory state (like the `Mutex<Vec<Note>>` of the example) for caches only;
real data goes to a database or another service. With `lesto::db`, build the pool once in
`main` and reuse it across invocations, as the example does with its state.

## Tasks after the response

Chapter 17's `BackgroundTasks` works in Lambda, with one guarantee the platform can keep: the
task runs to completion unless the invocation's deadline or a crash ends it first. It is still
best effort, not durable (a timeout loses the task and nothing retries it): work that must happen
goes to a queue.

**How it works.** Lambda freezes the environment when the runtime *and* every extension have
asked for their next event. lesto registers an internal extension, named `lesto`, for `INVOKE`
events. It needs the `lambda` feature only (not `otel`) and is registered whenever the function
handles one invocation at a time, because whether the application spawns tasks is not known at
init, and extensions can only register then. Per invocation, lesto keeps the tasks of that
request in a scope of their own, runs the router, and posts the response *first*. Then the
extension waits for the tasks, exports the telemetry (with `otel`: the tasks' spans and logs
belong to the same trace) and only then asks for its next event, which is what lets Lambda
freeze. Without tasks and without telemetry it answers at once, and the cost is one local call
per invocation.

**The deadline.** The wait for the tasks ends 500 ms before the invocation's deadline, so the
extension still has time to flush and to log a warning that the invocation ended with tasks still
running. Tasks past that point are not cancelled: they are frozen with the environment and
resume if it is thawed for another invocation, or die with it. Set the function's timeout for
the slowest request plus its slowest task.

**Billing.** The time after the response is billed and counts against the function timeout. It
shows in the `REPORT` line as `PostRuntimeExtensionsDuration`.

**Capacity.** Lambda does not send the next invocation to an environment whose extension is
busy. Slow tasks lower the number of invocations each environment serves, so Lambda scales out:
more environments, more cold starts. The telemetry flush does this too, on a small scale; tasks
can make it large.

**Fallbacks.**

- *No Extensions API* (some emulators): lesto waits for the tasks *before* returning the
  response, as it does for the telemetry flush. Correct, but the caller waits. A warning at init
  says so.
- *Managed concurrency* (`AWS_LAMBDA_MAX_CONCURRENCY` above 1): invocations are not frozen between
  one another and cannot be paired with their responses, so there is no extension and no
  per-invocation scope. Tasks behave as on a server: they run on the process, best effort.
- *Tests*: `lambda::test::invoke` waits for the tasks before returning.

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

## Tracing with X-Ray, step by step

The whole recipe for an API on Lambda whose traces show up in X-Ray as one trace per request
(API Gateway, the Lambda segment and lesto's spans), with no collector layer. Chapter 15 explains
each piece.

**1. Features.** `lambda` and `otel`:

```toml
lesto = { version = "0.3", features = ["lambda", "otel"] }
```

**2. `main`.** Install the telemetry *before* `serve`, keep the guard, and ask for the X-Ray
parent:

```rust
#[lesto::main]
async fn main() -> Result<(), lesto::lambda::Error> {
    let _telemetry = lesto::otel::init_named("notes-api");
    lesto::lambda::serve_with(build_app(), Options::default().xray(true)).await
}
```

The order matters: install the telemetry before `serve_with`, so that what the extension
flushes after each response exists when the first invocation arrives.

**3. Environment of the function.** Spans go straight to the X-Ray OTLP endpoint; lesto signs the
requests with SigV4, using the credentials of the function's role that Lambda already puts in the
environment:

```sh
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=https://xray.eu-west-1.amazonaws.com/v1/traces
OTEL_LOGS_EXPORTER=none        # stdout already goes to CloudWatch Logs
OTEL_METRICS_EXPORTER=none     # or OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=https://monitoring.eu-west-1.amazonaws.com/v1/metrics
```

Turn logs and metrics off (or give them an endpoint): with only the traces endpoint set, their
exporters would fall back to `localhost`. `OTEL_SERVICE_NAME` overrides the name given to
`init_named`.

**4. AWS, once.**

- Turn on **Transaction Search** in the CloudWatch settings of the account: the X-Ray OTLP
  endpoint requires it. Spans are stored in the `aws/spans` log group and billed as CloudWatch
  Logs ingestion.
- Give the function's role **`AWSXrayWriteOnlyPolicy`**.
- Turn on **active tracing** on the function (and on API Gateway, for its segment), so that
  `xray(true)` has a segment to attach to.

With SAM, steps 3 and 4 (except Transaction Search) are:

```yaml
NotesApi:
  Type: AWS::Serverless::Function
  Properties:
    CodeUri: target/lambda/notes-api/
    Runtime: provided.al2023
    Architectures: [arm64]
    Handler: bootstrap
    Tracing: Active
    Policies: [AWSXrayWriteOnlyPolicy]
    Environment:
      Variables:
        OTEL_EXPORTER_OTLP_TRACES_ENDPOINT: !Sub https://xray.${AWS::Region}.amazonaws.com/v1/traces
        OTEL_LOGS_EXPORTER: none
        OTEL_METRICS_EXPORTER: none
    Events:
      Api:
        Type: HttpApi
```

**5. Check it.**

- The init logs of the function list the extension, `lesto`, registered for `INVOKE`. A warning
  `the Lambda Extensions API is not available` means lesto fell back to flushing before each
  response.
- The `REPORT` line of each invocation carries `PostRuntimeExtensionsDuration`: the export, done
  after the response.
- CloudWatch → X-Ray traces shows one trace per request: API Gateway, the Lambda segment, and
  lesto's `GET /notes/{id}` span under it.
- A `403` from the endpoint, logged by the exporter, means the policy or Transaction Search is
  missing.

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
- Logs go to CloudWatch through stdout, as JSON when the function's log format is JSON.
- An internal extension, `lesto`, holds the freeze until the invocation's background tasks are
  done and, with `otel`, its telemetry is flushed (`Options::flush_telemetry`): both after the
  response is posted.
- `BackgroundTasks` (chapter 17) works in Lambda, bound by the invocation's deadline, billed as
  `PostRuntimeExtensionsDuration`; `lambda::test::invoke` waits for the tasks.
- X-Ray with no layer: `otel` + `init_named` before `serve_with(.., Options::default().xray(true))`,
  the traces endpoint in the environment, Transaction Search, `AWSXrayWriteOnlyPolicy`, active
  tracing.
- Build with `cargo lambda build --release --arm64`, deploy with `cargo lambda deploy` or your
  infrastructure tool.
- Test the Lambda path in-process with `lesto::lambda::test::invoke` and event fixtures.

Next: [Observability: tracing and OpenTelemetry](15-observability.md).

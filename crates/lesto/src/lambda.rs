//! Run a lesto application on AWS Lambda (feature `lambda`).
//!
//! ```ignore
//! #[lesto::main]
//! async fn main() -> Result<(), lesto::lambda::Error> {
//!     lesto::lambda::serve(build_app().with_state(state)).await
//! }
//! ```
//!
//! [`serve`] starts the Lambda runtime when the process runs inside Lambda (the
//! `AWS_LAMBDA_RUNTIME_API` variable is set) and otherwise behaves like `App::serve` (address
//! from `LESTO_HOST`/`LESTO_PORT`), so the same binary works in production, under `lesto dev`,
//! and in tests. Events from API Gateway REST APIs (payload v1), HTTP APIs and Function URLs
//! (payload v2) and Application Load Balancers are all accepted.
//!
//! With a REST API the stage name is part of the path (`/prod/notes`); by default it is
//! removed so routes stay `/notes`. See [`Options::keep_stage`].
//!
//! Lambda freezes the environment once the response is posted, which would freeze the
//! [`BackgroundTasks`](crate::BackgroundTasks) a handler started. So lesto registers an internal
//! extension (named `lesto`) at init, whenever the function runs one invocation at a time: the
//! response goes back at once, and the extension holds Lambda's freeze back until the
//! invocation's tasks have finished, at most until half a second before the invocation's
//! deadline (the tasks still running then are frozen with the environment and logged with a
//! warning). Each invocation has a scope of its own, so only its tasks are waited for. With the
//! `otel` feature the same extension then exports the telemetry the invocation and its tasks
//! buffered, since the batch exporters' timers do not run while the environment is frozen; see
//! [`Options::flush_telemetry`]. Without the Extensions API (an emulator), or in
//! [`test::invoke`], the tasks (and the flush) are awaited before the response instead; with
//! managed concurrency (`AWS_LAMBDA_MAX_CONCURRENCY` above 1) nothing is waited for and tasks
//! run as they do on a server. The time after the response is billed.
//!
//! [`test::invoke`] feeds an event fixture to a router without Lambda, for tests.

use std::time::{Duration, SystemTime};

use crate::App;
use crate::background::Tracker;
use axum::Router;
use axum::body::Body;
use http::{Request, Response};
use http_body_util::BodyExt;
use lambda_http::RequestExt;
use lambda_http::request::RequestContext;
use tower::{Service, ServiceExt};

/// The error type of the Lambda runtime: any `Box<dyn std::error::Error + Send + Sync>`.
pub use lambda_http::Error;

/// How [`serve_with`] behaves inside Lambda.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// Keep the API Gateway stage in the request path (`/prod/notes`). Off by default: the
    /// stage is stripped and routes match as they do locally.
    pub keep_stage: bool,
    /// Flush the OpenTelemetry providers after every invocation, before Lambda freezes the
    /// environment (feature `otel`; without it there is nothing to flush). On by default.
    pub flush_telemetry: bool,
    /// Make the X-Ray trace of the invocation the parent of the request span (feature `otel`).
    /// Off by default.
    pub xray: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            keep_stage: false,
            flush_telemetry: true,
            xray: false,
        }
    }
}

impl Options {
    /// Keep (`true`) or strip (`false`, the default) the API Gateway stage.
    pub fn keep_stage(mut self, keep: bool) -> Self {
        self.keep_stage = keep;
        self
    }

    /// Flush (`true`, the default) or not the OpenTelemetry providers after each invocation.
    ///
    /// The flush exports what the providers of the live `lesto::otel::Telemetry` buffered
    /// (`lesto::otel::init`, or `Telemetry::from_providers` for providers of your own), including
    /// the spans and logs of the invocation's background tasks. It runs in the internal Lambda
    /// extension registered by [`run_with`], after the runtime has posted the response and the
    /// tasks have finished: the caller does not wait for it, the invocation's billed duration
    /// does. Without the Extensions API, or with several invocations per process
    /// (`AWS_LAMBDA_MAX_CONCURRENCY`), it runs before the response instead, as it does in
    /// [`test::invoke`]. Turn it off when the export goes through something that buffers across
    /// freezes by itself. This only concerns the telemetry: the extension is registered, and
    /// the tasks are waited for, either way.
    pub fn flush_telemetry(mut self, flush: bool) -> Self {
        self.flush_telemetry = flush;
        self
    }

    /// Continue the X-Ray trace of the invocation (`true`) or not (`false`, the default).
    ///
    /// With active tracing on, Lambda opens a segment for every invocation and hands its id to
    /// the function. With this option the request span becomes a child of that segment, so the
    /// backend shows API Gateway, the Lambda segment and lesto's spans as one trace instead of
    /// two. The id is read from the invocation context (`Lambda-Runtime-Trace-Id`), or else from
    /// the `X-Amzn-Trace-Id` header that API Gateway and ALB add; it replaces a `traceparent`.
    ///
    /// X-Ray's sampling decision is the parent's: with `Sampled=0` (active tracing off, or a
    /// request X-Ray chose not to trace) the default parent-based sampler records nothing. Only
    /// the trace and parent ids are taken; the spans are still exported over OTLP, to a collector
    /// that forwards them to X-Ray (the ADOT Lambda layer), and need the `otel` feature.
    pub fn xray(mut self, xray: bool) -> Self {
        self.xray = xray;
        self
    }
}

/// Is this process running inside AWS Lambda?
pub fn in_lambda() -> bool {
    std::env::var_os("AWS_LAMBDA_RUNTIME_API").is_some()
}

/// In Lambda: run the app as the function handler. Elsewhere: `App::serve()`.
pub async fn serve(app: App<()>) -> Result<(), Error> {
    serve_with(app, Options::default()).await
}

/// [`serve`] with [`Options`].
pub async fn serve_with(app: App<()>, options: Options) -> Result<(), Error> {
    if in_lambda() {
        run_with(app.into_router(), options).await
    } else {
        app.serve().await.map_err(Error::from)
    }
}

/// [`serve`] with an explicit local address (`App::serve_at`) outside Lambda.
pub async fn serve_at(app: App<()>, addr: impl tokio::net::ToSocketAddrs) -> Result<(), Error> {
    if in_lambda() {
        run(app.into_router()).await
    } else {
        app.serve_at(addr).await.map_err(Error::from)
    }
}

/// Run an already built router as the Lambda handler (stage stripped).
pub async fn run(router: Router) -> Result<(), Error> {
    run_with(router, Options::default()).await
}

/// [`run`] with [`Options`].
pub async fn run_with(router: Router, options: Options) -> Result<(), Error> {
    // Lambda sends stdout to CloudWatch; `AWS_LAMBDA_LOG_FORMAT=JSON` makes it JSON.
    #[cfg(feature = "log")]
    crate::log::init();
    // Registered before the runtime starts: extensions belong to the init phase.
    let flush = Flush::start(options).await;
    lambda_http::run(with_options(router, options, flush)).await
}

/// When the work that follows an invocation's response happens: the background tasks it
/// started ([`crate::BackgroundTasks`]) and, with feature `otel`, the telemetry flush.
#[derive(Clone, Debug)]
enum Flush {
    /// Before the response is returned: tests, or no Extensions API. The invocation waits for
    /// its tasks, then flushes the telemetry.
    BeforeResponse,
    /// After it, by the internal extension, which the sender tells that an invocation's tasks
    /// are over.
    AfterResponse(tokio::sync::mpsc::UnboundedSender<()>),
    /// Managed concurrency: the instance is not frozen between invocations, so tasks run like
    /// on a server (process-wide scope, not waited for); the telemetry is flushed before the
    /// response.
    Concurrent,
}

impl Flush {
    /// In Lambda: the internal extension when it can be registered.
    async fn start(options: Options) -> Flush {
        if managed_concurrency() {
            return Flush::Concurrent;
        }
        let Some(api) = std::env::var("AWS_LAMBDA_RUNTIME_API").ok() else {
            return Flush::BeforeResponse;
        };
        match extension::start(&api, options.flush_telemetry).await {
            Some(done) => Flush::AfterResponse(done),
            None => Flush::BeforeResponse,
        }
    }

    /// Outside Lambda ([`test::invoke`]): there is no extension, so before the response.
    fn local() -> Flush {
        Flush::BeforeResponse
    }
}

/// More than one invocation per process (`AWS_LAMBDA_MAX_CONCURRENCY` above 1): the instance is
/// not frozen between invocations and `INVOKE` events would not pair with responses.
fn managed_concurrency() -> bool {
    std::env::var("AWS_LAMBDA_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .is_some_and(|n| n > 1)
}

/// Tells the extension the invocation is over when dropped, so an error or a panic on the way
/// still lets it flush and lets Lambda freeze the environment.
struct DoneOnDrop(Option<tokio::sync::mpsc::UnboundedSender<()>>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            // The receiver is gone only when the extension task is: nothing to tell then.
            let _ = done.send(());
        }
    }
}

/// The router as Lambda sees it: one invocation is the request mapped by [`invoke_once`].
fn with_options(
    router: Router,
    options: Options,
    flush: Flush,
) -> impl Service<
    lambda_http::Request,
    Response = Response<Body>,
    Error = Error,
    Future = impl Future<Output = Result<Response<Body>, Error>> + Send,
> + Clone {
    tower::service_fn(move |req: lambda_http::Request| {
        invoke_once(router.clone(), req, options, flush.clone())
    })
}

/// How long before the deadline the wait for tasks gives up, to leave the extension the time to
/// flush the telemetry and to log what is still running.
const DEADLINE_MARGIN: Duration = Duration::from_millis(500);

/// The time left to wait for tasks: until `deadline_ms` (milliseconds since the epoch) minus
/// [`DEADLINE_MARGIN`], zero when that has passed. `None` without a deadline.
fn wait_budget(deadline_ms: Option<u64>) -> Option<Duration> {
    let deadline = Duration::from_millis(deadline_ms.filter(|ms| *ms > 0)?);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    Some(deadline.saturating_sub(now).saturating_sub(DEADLINE_MARGIN))
}

/// Waits for the invocation's tasks, at most `budget`; logs how many are left when it runs out.
async fn wait_for_tasks(tracker: &Tracker, budget: Option<Duration>) {
    match budget {
        None => tracker.wait().await,
        Some(budget) => {
            if tokio::time::timeout(budget, tracker.wait()).await.is_err() {
                tracing::warn!(
                    running = tracker.live(),
                    "the invocation ends with background tasks still running; \
                     Lambda freezes them with the environment"
                );
            }
        }
    }
}

/// One invocation: strip the stage, run the router, collect the body, then the work after the
/// response (tasks, telemetry flush).
///
/// The path is rewritten *before* the router sees it (a `Router::layer` would run after
/// routing), and here rather than through the runtime's `AWS_LAMBDA_HTTP_IGNORE_STAGE_IN_PATH`
/// variable: the option stays local to this router (two routers, two settings, one process)
/// and nothing mutates the environment of a running multi-threaded program.
///
/// The body is collected here (`lambda_http` buffers it anyway) so the work after the response
/// comes after everything the handler logs while producing it, and the request span is closed
/// by then. Each invocation has a [`Tracker`] of its own (unless the instance runs several
/// invocations at once): with the extension a spawned waiter holds the "done" signal until the
/// tasks end, so the response goes back at once and Lambda freezes only afterwards; without it
/// the tasks are awaited before the response.
async fn invoke_once(
    router: Router,
    req: lambda_http::Request,
    options: Options,
    flush: Flush,
) -> Result<Response<Body>, Error> {
    let done = DoneOnDrop(match &flush {
        Flush::AfterResponse(done) => Some(done.clone()),
        _ => None,
    });
    let after_response = matches!(flush, Flush::AfterResponse(_));
    let tracker = match flush {
        Flush::Concurrent => None,
        _ => Some(Tracker::new()),
    };
    let mut req = if options.keep_stage {
        req
    } else {
        strip_stage(req)
    };
    let budget = wait_budget(req.lambda_context_ref().map(|context| context.deadline));
    #[cfg(feature = "otel")]
    if options.xray
        && let Some(parent) = xray::parent_of(&req)
    {
        req.extensions_mut()
            .insert(crate::trace::propagation::RemoteParent(parent));
    }
    if let Some(tracker) = &tracker {
        req.extensions_mut()
            .insert(crate::background::BackgroundScope(tracker.clone()));
    }
    let response = match router.oneshot(req).await {
        Ok(response) => response,
        Err(infallible) => match infallible {},
    };
    let (parts, body) = response.into_parts();
    let collected = body.collect().await;
    if let Some(tracker) = tracker {
        if after_response {
            // The runtime posts the response now; the extension is told when the tasks end.
            tokio::spawn(async move {
                wait_for_tasks(&tracker, budget).await;
                drop(done);
            });
        } else {
            wait_for_tasks(&tracker, budget).await;
        }
    }
    let bytes = collected?.to_bytes();
    #[cfg(feature = "otel")]
    if options.flush_telemetry && !after_response {
        crate::otel::flush_async().await;
    }
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

/// Remove the API Gateway stage prefix that `lambda_http` puts in the path.
fn strip_stage<B>(mut req: Request<B>) -> Request<B> {
    if let Some(stage) = stage_of(&req) {
        let prefix = format!("/{stage}");
        let path = req.uri().path();
        let stripped = if path == prefix {
            Some("/".to_string())
        } else {
            path.strip_prefix(&prefix)
                .filter(|rest| rest.starts_with('/'))
                .map(str::to_owned)
        };
        if let Some(stripped) = stripped {
            let mut parts = req.uri().clone().into_parts();
            let path_and_query = match req.uri().query() {
                Some(query) => format!("{stripped}?{query}"),
                None => stripped,
            };
            if let Ok(pq) = path_and_query.parse() {
                parts.path_and_query = Some(pq);
                if let Ok(uri) = http::Uri::from_parts(parts) {
                    *req.uri_mut() = uri;
                }
            }
        }
    }
    req
}

/// The deployment stage of an API Gateway event, when it is part of the path.
fn stage_of<B>(req: &Request<B>) -> Option<String> {
    let stage = match req.request_context_ref()? {
        RequestContext::ApiGatewayV1(ctx) => ctx.stage.clone(),
        RequestContext::ApiGatewayV2(ctx) => ctx.stage.clone(),
        _ => None,
    }?;
    // The `$default` stage of an HTTP API has no path prefix.
    (!stage.is_empty() && stage != "$default").then_some(stage)
}

/// The internal Lambda extension: it holds Lambda's freeze back until an invocation's
/// background tasks are over, and flushes the telemetry then (feature `otel`).
///
/// Lambda freezes the environment once the runtime *and* every extension have asked for the next
/// event. An extension registered for `INVOKE` events therefore gets time after the runtime has
/// posted the response: the caller has its answer, and the tasks and the export happen before
/// the freeze. The protocol is two calls (`register`, then `event/next` in a loop), written here
/// on `lambda_runtime_api_client`, the client the runtime itself uses, rather than through
/// `lambda-extension`, which would add `chrono`, `async-stream` and `tokio-stream` for them.
/// <https://docs.aws.amazon.com/lambda/latest/dg/runtimes-extensions-api.html>
mod extension {
    use std::time::Duration;

    use http_body_util::BodyExt;
    use lambda_runtime_api_client::{Client, body::Body};
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

    /// Unique among the function's extensions.
    pub(super) const NAME: &str = "lesto";

    /// Register with the Extensions API at `api` (`host:port`) and run the extension loop.
    /// `None`, and a warning, when the API is not there (an emulator without it).
    /// `flush_telemetry`: export the telemetry after each invocation (feature `otel`).
    pub(super) async fn start(api: &str, flush_telemetry: bool) -> Option<UnboundedSender<()>> {
        let client = match format!("http://{api}")
            .parse()
            .map_err(|e: http::uri::InvalidUri| e.to_string())
            .and_then(|uri| {
                Client::builder()
                    .with_endpoint(uri)
                    .build()
                    .map_err(|e| e.to_string())
            }) {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(%error, "the Lambda runtime API address is not usable");
                return None;
            }
        };
        let id = match register(&client).await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the Lambda Extensions API is not available; background tasks and telemetry are awaited before each response"
                );
                return None;
            }
        };
        let (done, invocations) = unbounded_channel();
        tokio::spawn(run(client, id, invocations, flush_telemetry));
        Some(done)
    }

    async fn register(client: &Client) -> Result<String, String> {
        let request = lambda_runtime_api_client::build_request()
            .method("POST")
            .uri("/2020-01-01/extension/register")
            .header("Lambda-Extension-Name", NAME)
            .header("content-type", "application/json")
            .body(Body::from(r#"{"events":["INVOKE"]}"#))
            .map_err(|e| e.to_string())?;
        let response = client.call(request).await.map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("register answered {}", response.status()));
        }
        response
            .headers()
            .get("Lambda-Extension-Identifier")
            .and_then(|id| id.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| "register answered without Lambda-Extension-Identifier".into())
    }

    /// Ask for the next event: this is also what tells Lambda the previous one is finished.
    async fn next(client: &Client, id: &str) -> Result<(), String> {
        let request = lambda_runtime_api_client::build_request()
            .method("GET")
            .uri("/2020-01-01/extension/event/next")
            .header("Lambda-Extension-Identifier", id)
            .body(Body::empty())
            .map_err(|e| e.to_string())?;
        let response = client.call(request).await.map_err(|e| e.to_string())?;
        let (parts, body) = response.into_parts();
        // The body says which invocation; the channel already pairs them one to one.
        let _ = body.collect().await;
        if parts.status.is_success() {
            Ok(())
        } else {
            Err(format!("event/next answered {}", parts.status))
        }
    }

    async fn run(
        client: Client,
        id: String,
        mut invocations: UnboundedReceiver<()>,
        flush_telemetry: bool,
    ) {
        let _ = flush_telemetry;
        loop {
            // An extension that stops asking holds every invocation until its timeout, so a
            // failed call is retried, never given up on.
            if let Err(error) = next(&client, &id).await {
                tracing::warn!(%error, "the Lambda extension could not get its next event");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            // An INVOKE: wait until the runtime has built the response and the invocation's
            // tasks are over, then export.
            if invocations.recv().await.is_none() {
                return;
            }
            loop {
                #[cfg(feature = "otel")]
                if flush_telemetry {
                    crate::otel::flush_async().await;
                }
                // Invocations that ended during the export are exported too before asking for
                // more. Lambda never starts one while an extension is busy, but an emulator
                // that does (floci) would otherwise leave their telemetry for the next event.
                let mut more = false;
                while invocations.try_recv().is_ok() {
                    more = true;
                }
                if !more {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod extension_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use lambda_http::RequestExt;
    use tokio::sync::{Mutex, Notify, mpsc};

    use super::{Flush, Options, extension, invoke_once, wait_budget};
    use crate::background::BackgroundTasks;

    /// A stand-in for the Extensions API: reports every `event/next` call and answers it with
    /// the next event the test pushes.
    async fn extensions_api() -> (
        String,
        mpsc::UnboundedReceiver<()>,
        mpsc::UnboundedSender<()>,
    ) {
        let (asked, asked_rx) = mpsc::unbounded_channel::<()>();
        let (invoke, invokes) = mpsc::unbounded_channel::<()>();
        let invokes = Arc::new(Mutex::new(invokes));
        let app = Router::new()
            .route(
                "/2020-01-01/extension/register",
                post(|headers: HeaderMap, body: String| async move {
                    assert_eq!(headers["lambda-extension-name"], extension::NAME);
                    assert_eq!(body, r#"{"events":["INVOKE"]}"#);
                    ([("lambda-extension-identifier", "ext-1")], "{}")
                }),
            )
            .route(
                "/2020-01-01/extension/event/next",
                get(move |headers: HeaderMap| async move {
                    assert_eq!(headers["lambda-extension-identifier"], "ext-1");
                    asked.send(()).unwrap();
                    invokes.lock().await.recv().await;
                    (StatusCode::OK, r#"{"eventType":"INVOKE","requestId":"r"}"#)
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (address, asked_rx, invoke)
    }

    async fn wait(rx: &mut mpsc::UnboundedReceiver<()>) {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the extension asked for its next event");
    }

    /// A route that starts one task, which ends when `release` is notified.
    fn router(release: Arc<Notify>) -> Router {
        Router::new().route(
            "/",
            get(move |tasks: BackgroundTasks| {
                let release = release.clone();
                async move {
                    tasks.spawn(async move { release.notified().await });
                    "ok"
                }
            }),
        )
    }

    fn request(deadline_ms: Option<u64>) -> lambda_http::Request {
        let request = lambda_http::http::Request::get("/")
            .body(lambda_http::Body::Empty)
            .unwrap();
        match deadline_ms {
            Some(deadline) => {
                let mut context = lambda_http::Context::default();
                context.deadline = deadline;
                request.with_lambda_context(context)
            }
            None => request,
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[tokio::test]
    async fn the_response_comes_first_and_done_only_after_the_task() {
        let release = Arc::new(Notify::new());
        let (api, mut asked, invoke) = extensions_api().await;
        let done = extension::start(&api, true).await.expect("registered");
        // The first `next` ends the init phase.
        wait(&mut asked).await;
        invoke.send(()).unwrap();

        let response = invoke_once(
            router(release.clone()),
            request(None),
            Options::default(),
            Flush::AfterResponse(done),
        )
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            200,
            "the response did not wait for the task"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), asked.recv())
                .await
                .is_err(),
            "no next event while the task runs: Lambda would freeze it"
        );

        release.notify_one();
        wait(&mut asked).await;
    }

    #[tokio::test]
    async fn the_wait_stops_half_a_second_before_the_deadline() {
        let release = Arc::new(Notify::new());
        let (api, mut asked, invoke) = extensions_api().await;
        let done = extension::start(&api, true).await.expect("registered");
        wait(&mut asked).await;
        invoke.send(()).unwrap();

        // 700 ms left: the waiter gives up after about 200 ms; the task never ends.
        let started = std::time::Instant::now();
        invoke_once(
            router(release),
            request(Some(now_ms() + 700)),
            Options::default(),
            Flush::AfterResponse(done),
        )
        .await
        .unwrap();
        wait(&mut asked).await;
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(150), "{waited:?}");
        assert!(waited < Duration::from_millis(700), "{waited:?}");
    }

    #[tokio::test]
    async fn without_the_extension_the_tasks_end_before_the_response() {
        let release = Arc::new(Notify::new());
        let invocation = tokio::spawn(invoke_once(
            router(release.clone()),
            request(None),
            Options::default(),
            Flush::BeforeResponse,
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!invocation.is_finished(), "the response waits for the task");
        release.notify_one();
        let response = invocation.await.unwrap().unwrap();
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn managed_concurrency_does_not_wait() {
        let release = Arc::new(Notify::new());
        let response = invoke_once(
            router(release.clone()),
            request(None),
            Options::default(),
            Flush::Concurrent,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 200);
        release.notify_one();
    }

    #[test]
    fn the_budget_is_the_deadline_minus_the_margin() {
        assert_eq!(wait_budget(None), None);
        assert_eq!(wait_budget(Some(0)), None);
        assert_eq!(wait_budget(Some(1)), Some(Duration::ZERO), "long past");
        let budget = wait_budget(Some(now_ms() + 10_500)).unwrap();
        assert!(
            budget > Duration::from_secs(9) && budget <= Duration::from_secs(10),
            "{budget:?}"
        );
        assert_eq!(wait_budget(Some(now_ms() + 100)), Some(Duration::ZERO));
    }

    #[tokio::test]
    async fn no_extensions_api_means_no_extension() {
        // A port nobody listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);
        assert!(extension::start(&address, true).await.is_none());
    }

    #[cfg(feature = "otel")]
    #[tokio::test]
    async fn the_extension_flushes_after_the_invocation_and_before_the_next_event() {
        use opentelemetry::trace::{Tracer as _, TracerProvider as _};
        use opentelemetry_sdk::trace::{
            BatchConfigBuilder, BatchSpanProcessor, InMemorySpanExporter, SdkTracerProvider,
        };

        use crate::otel::Telemetry;

        let exporter = InMemorySpanExporter::default();
        let processor = BatchSpanProcessor::builder(exporter.clone())
            .with_batch_config(
                BatchConfigBuilder::default()
                    .with_scheduled_delay(Duration::from_secs(3600))
                    .build(),
            )
            .build();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(processor)
            .build();
        let tracer = provider.tracer("test");
        let telemetry = Telemetry::from_providers(Some(provider), None, None);
        let (api, mut asked, invoke) = extensions_api().await;
        let done = extension::start(&api, true).await.expect("registered");
        wait(&mut asked).await;

        invoke.send(()).unwrap();
        tracer.in_span("handler", |_| {});
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            exporter.get_finished_spans().unwrap().is_empty(),
            "nothing is exported while the invocation runs"
        );

        // The response is built: the extension exports, then asks for the next event, which is
        // what lets Lambda freeze the environment.
        done.send(()).unwrap();
        wait(&mut asked).await;
        assert_eq!(exporter.get_finished_spans().unwrap().len(), 1);

        telemetry.shutdown();
    }
}

/// The X-Ray trace header, as an OpenTelemetry parent (feature `otel`).
#[cfg(feature = "otel")]
mod xray {
    use lambda_http::RequestExt;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
    };

    /// The X-Ray trace of the invocation: the Lambda context first (the segment of this
    /// invocation), the `X-Amzn-Trace-Id` header of API Gateway or ALB otherwise.
    pub(super) fn parent_of(req: &lambda_http::Request) -> Option<opentelemetry::Context> {
        let from_context = req
            .lambda_context_ref()
            .and_then(|context| context.xray_trace_id.as_deref());
        let from_header = || {
            req.headers()
                .get("x-amzn-trace-id")
                .and_then(|value| value.to_str().ok())
        };
        let span_context = from_context
            .and_then(parse)
            .or_else(|| from_header().and_then(parse))?;
        Some(opentelemetry::Context::new().with_remote_span_context(span_context))
    }

    /// `Root=1-{8 hex epoch}-{24 hex};Parent={16 hex};Sampled=1`, in any order, other keys
    /// (`Lineage`, `Self`) ignored. `None` without a valid root and parent: a root alone names
    /// a trace but no span to hang from.
    pub(super) fn parse(header: &str) -> Option<SpanContext> {
        let mut trace_id = None;
        let mut span_id = None;
        let mut flags = TraceFlags::default();
        for field in header.split(';') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key.trim() {
                "Root" => trace_id = root(value.trim()),
                "Parent" => {
                    span_id = hex::<16>(value.trim()).and_then(|id| {
                        SpanId::from_hex(id)
                            .ok()
                            .filter(|id| *id != SpanId::INVALID)
                    })
                }
                "Sampled" if value.trim() == "1" => flags = TraceFlags::SAMPLED,
                _ => {}
            }
        }
        Some(SpanContext::new(
            trace_id?,
            span_id?,
            flags,
            true,
            TraceState::default(),
        ))
    }

    /// `1-5759e988-bd862e3fe1be46a994272793` is the W3C trace id
    /// `5759e988bd862e3fe1be46a994272793`.
    fn root(value: &str) -> Option<TraceId> {
        let mut parts = value.split('-');
        let (Some("1"), Some(epoch), Some(random), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let id = format!("{}{}", hex::<8>(epoch)?, hex::<24>(random)?);
        TraceId::from_hex(&id)
            .ok()
            .filter(|id| *id != TraceId::INVALID)
    }

    /// `value` if it is exactly `N` hexadecimal digits.
    fn hex<const N: usize>(value: &str) -> Option<&str> {
        (value.len() == N && value.bytes().all(|b| b.is_ascii_hexdigit())).then_some(value)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_lambda_header_becomes_a_remote_parent() {
            let context = parse(
                "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1;Lineage=a87bd80c:0",
            )
            .unwrap();
            assert_eq!(
                context.trace_id().to_string(),
                "5759e988bd862e3fe1be46a994272793"
            );
            assert_eq!(context.span_id().to_string(), "53995c3f42cd8ad8");
            assert!(context.is_sampled());
            assert!(context.is_remote());
        }

        #[test]
        fn order_and_spaces_do_not_matter_and_unsampled_stays_unsampled() {
            let context = parse(
                "Sampled=0; Parent=53995c3f42cd8ad8; Root=1-5759e988-bd862e3fe1be46a994272793",
            )
            .unwrap();
            assert!(!context.is_sampled());
            assert!(
                parse("Root=1-5759e988-bd862e3fe1be46a994272793;Sampled=?;Parent=53995c3f42cd8ad8")
                    .is_some_and(|c| !c.is_sampled()),
                "a deferred decision is not a yes"
            );
        }

        #[test]
        fn malformed_headers_give_no_parent() {
            for header in [
                "",
                "Root=1-5759e988-bd862e3fe1be46a994272793",
                "Parent=53995c3f42cd8ad8;Sampled=1",
                "Root=2-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8",
                "Root=1-5759e98-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8",
                "Root=1-5759e988-bd862e3fe1be46a99427279z;Parent=53995c3f42cd8ad8",
                "Root=1-5759e988-bd862e3fe1be46a994272793-00;Parent=53995c3f42cd8ad8",
                "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f",
                "Root=1-00000000-000000000000000000000000;Parent=53995c3f42cd8ad8",
                "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=0000000000000000",
                "Root=1-5759e988-bd862e3fe1be46a99427279é;Parent=53995c3f42cd8ad8",
            ] {
                assert!(parse(header).is_none(), "{header}");
            }
        }
    }
}

/// Test helpers: drive a router with Lambda event fixtures, no Lambda needed.
pub mod test {
    use axum::Router;
    use axum::body::Bytes;
    use http::Response;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::{Error, Options, with_options};

    /// Convert an API Gateway (v1 or v2), Function URL or ALB event to an HTTP request, run
    /// it through `router`, and return the response with its body collected, after the
    /// invocation's [`BackgroundTasks`](crate::BackgroundTasks) have finished: a test can
    /// assert their effects right after the call.
    ///
    /// Applies [`Options::default`] (stage stripped, telemetry flushed as in Lambda);
    /// [`invoke_with`] takes explicit options.
    pub async fn invoke(router: &Router, event_json: &str) -> Result<Response<Bytes>, Error> {
        invoke_with(router, event_json, Options::default()).await
    }

    /// [`invoke`] with [`Options`].
    pub async fn invoke_with(
        router: &Router,
        event_json: &str,
        options: Options,
    ) -> Result<Response<Bytes>, Error> {
        let request = lambda_http::request::from_str(event_json)?;
        let response = with_options(router.clone(), options, super::Flush::local())
            .oneshot(request)
            .await?;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await?.to_bytes();
        Ok(Response::from_parts(parts, bytes))
    }
}

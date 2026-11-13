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
//! With the `otel` feature, the telemetry buffered during an invocation is exported before Lambda
//! freezes the environment, by an internal extension once the response is posted: the batch
//! exporters' timers do not run while the environment is frozen. See
//! [`Options::flush_telemetry`].
//!
//! [`test::invoke`] feeds an event fixture to a router without Lambda, for tests.

use crate::App;
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
    /// (`lesto::otel::init`, or `Telemetry::from_providers` for providers of your own). It runs
    /// in an internal Lambda extension registered by [`run_with`], after the runtime has posted
    /// the response: the caller does not wait for it, the invocation's billed duration does.
    /// Without the Extensions API, or with several invocations per process
    /// (`AWS_LAMBDA_MAX_CONCURRENCY`), it runs before the response instead, as it does in
    /// [`test::invoke`]. Turn it off when the export goes through something that buffers across
    /// freezes by itself.
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

/// When an invocation's telemetry is exported.
#[derive(Clone, Debug)]
enum Flush {
    /// Not at all: `flush_telemetry(false)`, or no `otel` feature.
    Off,
    /// Before the response is returned: tests, managed concurrency, or no Extensions API.
    #[cfg(feature = "otel")]
    BeforeResponse,
    /// After it, by the internal extension, which the sender tells that an invocation is done.
    #[cfg(feature = "otel")]
    AfterResponse(tokio::sync::mpsc::UnboundedSender<()>),
}

impl Flush {
    /// In Lambda: the internal extension when it can be registered.
    async fn start(options: Options) -> Flush {
        if !options.flush_telemetry {
            return Flush::Off;
        }
        #[cfg(feature = "otel")]
        {
            match extension::start_from_env().await {
                Some(done) => Flush::AfterResponse(done),
                None => Flush::BeforeResponse,
            }
        }
        #[cfg(not(feature = "otel"))]
        Flush::Off
    }

    /// Outside Lambda ([`test::invoke`]): there is no extension, so before the response.
    fn local(options: Options) -> Flush {
        #[cfg(feature = "otel")]
        if options.flush_telemetry {
            return Flush::BeforeResponse;
        }
        let _ = options;
        Flush::Off
    }
}

/// Tells the extension the invocation is over when dropped, so an error or a panic on the way
/// still lets it flush and lets Lambda freeze the environment.
#[cfg(feature = "otel")]
struct DoneOnDrop(Option<tokio::sync::mpsc::UnboundedSender<()>>);

#[cfg(feature = "otel")]
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

/// One invocation: strip the stage, run the router, collect the body, flush the telemetry.
///
/// The path is rewritten *before* the router sees it (a `Router::layer` would run after
/// routing), and here rather than through the runtime's `AWS_LAMBDA_HTTP_IGNORE_STAGE_IN_PATH`
/// variable: the option stays local to this router (two routers, two settings, one process)
/// and nothing mutates the environment of a running multi-threaded program.
///
/// The body is collected here (`lambda_http` buffers it anyway) so the flush comes after
/// everything the handler logs while producing it, and the request span is closed by then.
async fn invoke_once(
    router: Router,
    req: lambda_http::Request,
    options: Options,
    flush: Flush,
) -> Result<Response<Body>, Error> {
    #[cfg(feature = "otel")]
    let _done = DoneOnDrop(match &flush {
        Flush::AfterResponse(done) => Some(done.clone()),
        _ => None,
    });
    #[cfg_attr(not(feature = "otel"), expect(unused_mut))]
    let mut req = if options.keep_stage {
        req
    } else {
        strip_stage(req)
    };
    #[cfg(feature = "otel")]
    if options.xray
        && let Some(parent) = xray::parent_of(&req)
    {
        req.extensions_mut()
            .insert(crate::trace::propagation::RemoteParent(parent));
    }
    let response = match router.oneshot(req).await {
        Ok(response) => response,
        Err(infallible) => match infallible {},
    };
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await?.to_bytes();
    #[cfg(feature = "otel")]
    if let Flush::BeforeResponse = flush {
        crate::otel::flush_async().await;
    }
    #[cfg(not(feature = "otel"))]
    let _ = flush;
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

/// An internal Lambda extension that flushes the telemetry after the response (feature `otel`).
///
/// Lambda freezes the environment once the runtime *and* every extension have asked for the next
/// event. An extension registered for `INVOKE` events therefore gets time after the runtime has
/// posted the response: the caller has its answer, and the export happens before the freeze.
/// The protocol is two calls (`register`, then `event/next` in a loop), written here on the
/// reqwest `otel` already brings rather than through `lambda-extension`, which would add
/// `chrono`, `async-stream` and `tokio-stream` for them.
/// <https://docs.aws.amazon.com/lambda/latest/dg/runtimes-extensions-api.html>
#[cfg(feature = "otel")]
mod extension {
    use std::time::Duration;

    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

    /// Unique among the function's extensions.
    pub(super) const NAME: &str = "lesto-telemetry-flush";

    /// Register when there is something to flush and one invocation at a time. Under managed
    /// concurrency (`AWS_LAMBDA_MAX_CONCURRENCY` above 1) the instance is not frozen between
    /// invocations and `INVOKE` events would not pair with responses: flush before the
    /// response there.
    pub(super) async fn start_from_env() -> Option<UnboundedSender<()>> {
        if !crate::otel::has_telemetry() {
            return None;
        }
        let concurrent = std::env::var("AWS_LAMBDA_MAX_CONCURRENCY")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .is_some_and(|n| n > 1);
        if concurrent {
            return None;
        }
        let api = std::env::var("AWS_LAMBDA_RUNTIME_API").ok()?;
        start(&api).await
    }

    /// Register with the Extensions API at `api` (`host:port`) and run the extension loop.
    /// `None`, and a warning, when the API is not there (an emulator without it).
    pub(super) async fn start(api: &str) -> Option<UnboundedSender<()>> {
        let client = reqwest::Client::new();
        let base = format!("http://{api}/2020-01-01/extension");
        let id = match register(&client, &base).await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the Lambda Extensions API is not available; telemetry is flushed before each response"
                );
                return None;
            }
        };
        let (done, invocations) = unbounded_channel();
        tokio::spawn(run(client, base, id, invocations));
        Some(done)
    }

    async fn register(client: &reqwest::Client, base: &str) -> Result<String, String> {
        let response = client
            .post(format!("{base}/register"))
            .header("Lambda-Extension-Name", NAME)
            .header("content-type", "application/json")
            .body(r#"{"events":["INVOKE"]}"#)
            .send()
            .await
            .map_err(|e| e.to_string())?;
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
    async fn next(client: &reqwest::Client, base: &str, id: &str) -> Result<(), String> {
        let response = client
            .get(format!("{base}/event/next"))
            .header("Lambda-Extension-Identifier", id)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        // The body says which invocation; the channel already pairs them one to one.
        let _ = response.bytes().await;
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("event/next answered {status}"))
        }
    }

    async fn run(
        client: reqwest::Client,
        base: String,
        id: String,
        mut invocations: UnboundedReceiver<()>,
    ) {
        loop {
            // An extension that stops asking holds every invocation until its timeout, so a
            // failed call is retried, never given up on.
            if let Err(error) = next(&client, &base, &id).await {
                tracing::warn!(%error, "the Lambda extension could not get its next event");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            // An INVOKE: wait until the runtime has built the response, then export.
            if invocations.recv().await.is_none() {
                return;
            }
            loop {
                crate::otel::flush_async().await;
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

#[cfg(all(test, feature = "otel"))]
mod extension_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use opentelemetry::trace::{Tracer as _, TracerProvider as _};
    use opentelemetry_sdk::trace::{
        BatchConfigBuilder, BatchSpanProcessor, InMemorySpanExporter, SdkTracerProvider,
    };
    use tokio::sync::{Mutex, mpsc};

    use super::extension;
    use crate::otel::Telemetry;

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

    #[tokio::test]
    async fn the_extension_flushes_after_the_invocation_and_before_the_next_event() {
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
        let done = extension::start(&api).await.expect("registered");
        // The first `next` ends the init phase.
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

    #[tokio::test]
    async fn no_extensions_api_means_no_extension() {
        // A port nobody listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);
        assert!(extension::start(&address).await.is_none());
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
    /// it through `router`, and return the response with its body collected.
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
        let response = with_options(router.clone(), options, super::Flush::local(options))
            .oneshot(request)
            .await?;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await?.to_bytes();
        Ok(Response::from_parts(parts, bytes))
    }
}

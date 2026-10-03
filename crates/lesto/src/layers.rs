//! The tower layers [`App::into_router`](crate::App::into_router) installs, usable on their own.
//!
//! An application that leaves lesto for a plain `axum::Router` (or that mounts a lesto router
//! inside a larger axum one) can keep the behavior it had by adding these:
//!
//! ```
//! use lesto::layers::{CatchPanicLayer, ProblemLayer};
//!
//! let router: axum::Router = axum::Router::new()
//!     .route("/hello", axum::routing::get(|| async { "hi" }))
//!     .layer(CatchPanicLayer)
//!     .layer(ProblemLayer);
//! ```
//!
//! Order matters: `Router::layer` makes the last one added the outermost, and
//! [`ProblemLayer`] belongs outside the panic catcher, so the `500` a caught panic produces
//! gets its `instance` too. [`RequestSpanLayer`] goes outside both, so the span sees the status
//! the client sees. They all run *inside* routing, which is what makes `MatchedPath` (and so
//! `http.route`) available.
//!
//! Every future here is a concrete type: no `Box::pin` on the happy path.

use std::any::Any;
use std::convert::Infallible;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use http::{Request, header};
use pin_project_lite::pin_project;
use tokio::task::futures::TaskLocalFuture;
use tower_layer::Layer;
use tower_service::Service;

use crate::error::{HttpError, PROBLEM_JSON, Problem, ProblemRendered, RenderContext};
use crate::trace::Trace;

// ---- problems -----------------------------------------------------------------------------

/// Finishes every RFC 9457 response of the requests below it: `instance` is the request path.
///
/// The work happens where the problem is *built* ([`Problem::into_response`]): the layer only
/// publishes the request path, so a problem is serialized once. A
/// `application/problem+json` response built by hand somewhere below — without the
/// [`ProblemRendered`] marker — still takes the slow path, where the body is buffered, parsed
/// and completed.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProblemLayer;

impl<S> Layer<S> for ProblemLayer {
    type Service = ProblemService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ProblemService { inner }
    }
}

/// The service [`ProblemLayer`] produces.
#[derive(Debug, Clone, Copy)]
pub struct ProblemService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for ProblemService<S>
where
    S: Service<Request<B>, Response = Response>,
{
    type Response = Response;
    type Error = S::Error;
    type Future = ProblemFuture<S::Future, S::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        // One `Uri` clone per request (reference-counted bytes); the path is only turned into
        // a `String` if a problem is actually rendered.
        let context = RenderContext {
            uri: req.uri().clone(),
        };
        ProblemFuture {
            state: ProblemState::Running {
                future: crate::error::RENDER.scope(context.clone(), self.inner.call(req)),
            },
            context,
            _error: PhantomData,
        }
    }
}

pin_project! {
    /// The future of [`ProblemService`].
    pub struct ProblemFuture<F, E> {
        #[pin]
        state: ProblemState<F>,
        context: RenderContext,
        _error: PhantomData<fn() -> E>,
    }
}

pin_project! {
    #[project = ProblemStateProj]
    enum ProblemState<F> {
        Running { #[pin] future: TaskLocalFuture<RenderContext, F> },
        // The slow path is a hand-written `problem+json` body being buffered and rewritten. It
        // is boxed: it costs an allocation on a path that already costs a parse, and it keeps
        // the common future small.
        Rewriting { future: Pin<Box<dyn Future<Output = Response> + Send>> },
    }
}

impl<F, E> Future for ProblemFuture<F, E>
where
    F: Future<Output = Result<Response, E>>,
{
    type Output = Result<Response, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            let rewriting = match this.state.as_mut().project() {
                ProblemStateProj::Running { future } => {
                    let response = std::task::ready!(future.poll(cx))?;
                    if response.extensions().get::<ProblemRendered>().is_some()
                        || !is_problem(&response)
                    {
                        return Poll::Ready(Ok(response));
                    }
                    Box::pin(rewrite(response, this.context.clone()))
                        as Pin<Box<dyn Future<Output = Response> + Send>>
                }
                ProblemStateProj::Rewriting { future } => {
                    return future.as_mut().poll(cx).map(Ok);
                }
            };
            this.state
                .set(ProblemState::Rewriting { future: rewriting });
        }
    }
}

fn is_problem(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.starts_with(PROBLEM_JSON))
}

/// Slow path: a `problem+json` response somebody else built. Buffer it, parse it and fill in
/// `instance`.
///
/// Problem bodies are produced by this process, never by the client, so buffering them whole is
/// bounded by what the application already built in memory.
async fn rewrite(response: Response, context: RenderContext) -> Response {
    let (mut parts, body) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        tracing::error!("problem+json body could not be read; answering without a body");
        return Response::from_parts(parts, Body::empty());
    };
    let Ok(mut problem) = serde_json::from_slice::<Problem>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    if problem.instance.is_none() {
        problem.instance = Some(context.uri.path().to_owned());
    }
    let body = serde_json::to_vec(&problem).unwrap_or_else(|_| bytes.to_vec());
    let mut response = Response::from_parts(parts, Body::from(body));
    response.extensions_mut().insert(ProblemRendered);
    response
}

// ---- panics -------------------------------------------------------------------------------

/// Turns a panic anywhere below (extractor, handler, inner middleware) into a `500` problem.
///
/// Without it hyper drops the connection and the client sees a transport error rather than a
/// response. The panic message is not sent to the client; the panic hook has already printed
/// it (and `tracing` gets a record).
#[derive(Debug, Clone, Copy)]
pub struct CatchPanicLayer;

impl<S> Layer<S> for CatchPanicLayer {
    type Service = CatchPanic<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CatchPanic { inner }
    }
}

/// The service [`CatchPanicLayer`] produces.
#[derive(Debug, Clone, Copy)]
pub struct CatchPanic<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for CatchPanic<S>
where
    S: Service<Request<B>, Response = Response, Error = Infallible>,
{
    type Response = Response;
    type Error = Infallible;
    type Future = CaughtFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        // A panic while *building* the future is caught too.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.inner.call(req))) {
            Ok(future) => CaughtFuture::Running { future },
            Err(payload) => CaughtFuture::Panicked {
                response: Some(panic_response(payload)),
            },
        }
    }
}

pin_project! {
    /// The future of [`CatchPanic`].
    #[project = CaughtFutureProj]
    // Variant fields are private in spirit; pin-project-lite takes no docs on them.
    #[allow(missing_docs)]
    pub enum CaughtFuture<F> {
        /// The inner future, not finished yet.
        Running { #[pin] future: F },
        /// The inner service panicked: the `500` problem, taken on the next poll.
        Panicked { response: Option<Response> },
    }
}

impl<F> Future for CaughtFuture<F>
where
    F: Future<Output = Result<Response, Infallible>>,
{
    type Output = Result<Response, Infallible>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            CaughtFutureProj::Running { future } => {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.poll(cx))) {
                    Ok(polled) => polled,
                    Err(payload) => Poll::Ready(Ok(panic_response(payload))),
                }
            }
            CaughtFutureProj::Panicked { response } => {
                Poll::Ready(Ok(response.take().expect("polled after completion")))
            }
        }
    }
}

fn panic_response(payload: Box<dyn Any + Send>) -> Response {
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string());
    tracing::error!(panic = %message, "handler panicked");
    HttpError::internal("Internal Server Error").into_response()
}

// ---- timeout ------------------------------------------------------------------------------

/// Answers `503 Service Unavailable` as a problem when the inner service takes longer than
/// `limit`; the handler's future is dropped at that point, which cancels it.
///
/// [`App::timeout`](crate::App::timeout) installs it. On a plain router, put it below
/// [`ProblemLayer`] so the problem gets its `instance`.
#[derive(Debug, Clone, Copy)]
pub struct TimeoutLayer {
    limit: Duration,
}

impl TimeoutLayer {
    /// Requests running longer than `limit` answer `503`.
    pub fn new(limit: Duration) -> Self {
        Self { limit }
    }
}

impl<S> Layer<S> for TimeoutLayer {
    type Service = Timeout<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Timeout {
            inner,
            limit: self.limit,
        }
    }
}

/// The service [`TimeoutLayer`] produces.
#[derive(Debug, Clone, Copy)]
pub struct Timeout<S> {
    inner: S,
    limit: Duration,
}

impl<S, B> Service<Request<B>> for Timeout<S>
where
    S: Service<Request<B>, Response = Response>,
{
    type Response = Response;
    type Error = S::Error;
    type Future = TimeoutFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        TimeoutFuture {
            future: self.inner.call(req),
            sleep: tokio::time::sleep(self.limit),
            limit: self.limit,
        }
    }
}

pin_project! {
    /// The future of [`Timeout`].
    pub struct TimeoutFuture<F> {
        #[pin]
        future: F,
        #[pin]
        sleep: tokio::time::Sleep,
        limit: Duration,
    }
}

impl<F, E> Future for TimeoutFuture<F>
where
    F: Future<Output = Result<Response, E>>,
{
    type Output = Result<Response, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        if let Poll::Ready(result) = this.future.poll(cx) {
            return Poll::Ready(result);
        }
        match this.sleep.poll(cx) {
            Poll::Ready(()) => {
                let limit_ms = this.limit.as_millis() as u64;
                tracing::warn!(limit_ms, "request timed out, answering 503");
                Poll::Ready(Ok(HttpError::new(
                    http::StatusCode::SERVICE_UNAVAILABLE,
                    format!("The request took longer than {limit_ms} ms"),
                )
                .into_response()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---- request span -------------------------------------------------------------------------

/// Opens the request span described in [`crate::trace`], records the response on it, and
/// writes the line per request (the access log: a `tracing` event with the target
/// `lesto::access`; the `log` module prints it).
///
/// Named `RequestSpanLayer` rather than `TraceLayer` so it can live next to
/// `tower_http::trace::TraceLayer` in one `use` list.
///
/// With no subscriber interested in the span or the line, the cost is the callsite checks
/// `tracing` does and nothing else: the future is the inner one, not wrapped, and the clock is
/// not read.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestSpanLayer {
    trace: Trace,
}

impl RequestSpanLayer {
    /// The default configuration ([`Trace::new`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Record what `trace` says. [`Trace::off`] leaves the request alone: the inner future is
    /// returned as it is, and the layer costs a branch.
    pub fn with(trace: Trace) -> Self {
        Self { trace }
    }
}

impl<S> Layer<S> for RequestSpanLayer {
    type Service = RequestSpan<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestSpan {
            inner,
            trace: self.trace,
        }
    }
}

/// The service [`RequestSpanLayer`] produces.
#[derive(Debug, Clone, Copy)]
pub struct RequestSpan<S> {
    inner: S,
    trace: Trace,
}

impl<S, B> Service<Request<B>> for RequestSpan<S>
where
    S: Service<Request<B>, Response = Response>,
{
    type Response = Response;
    type Error = S::Error;
    type Future = RequestSpanFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        #[cfg(feature = "otel")]
        let measured = crate::metrics::start(&req, self.trace);
        #[cfg(not(feature = "otel"))]
        let measured = ();
        let access = crate::trace::access_start(&req);
        let span = if self.trace.enabled {
            crate::trace::request_span(&req, self.trace)
        } else {
            tracing::Span::none()
        };
        if span.is_disabled() {
            return RequestSpanFuture::Disabled {
                future: self.inner.call(req),
                measured,
                access,
            };
        }
        let future = {
            let _entered = span.enter();
            self.inner.call(req)
        };
        RequestSpanFuture::Recording {
            future,
            span,
            measured,
            access,
        }
    }
}

/// The request's metric measurement: `Option<Pending>` with the `otel` feature, nothing
/// without it, so the future carries no extra field when there is nothing to record.
#[cfg(feature = "otel")]
type Measured = Option<crate::metrics::Pending>;
#[cfg(not(feature = "otel"))]
type Measured = ();

#[cfg(feature = "otel")]
fn finish_measure<E>(measured: &mut Measured, polled: &Result<Response, E>) {
    if let Some(pending) = measured.take() {
        crate::metrics::finish(pending, polled.as_ref().ok().map(|r| r.status()));
    }
}

#[cfg(not(feature = "otel"))]
fn finish_measure<E>(_: &mut Measured, _: &Result<Response, E>) {}

fn finish_access<E>(access: &mut Option<crate::trace::Access>, polled: &Result<Response, E>) {
    if let (Some(access), Ok(response)) = (access.take(), polled) {
        crate::trace::access_finish(access, response.status());
    }
}

pin_project! {
    /// The future of [`RequestSpan`].
    #[project = RequestSpanFutureProj]
    // Variant fields are private in spirit; pin-project-lite takes no docs on them.
    #[allow(missing_docs)]
    pub enum RequestSpanFuture<F> {
        /// Nobody is listening: the inner future, polled as if the layer were not there.
        Disabled { #[pin] future: F, measured: Measured, access: Option<crate::trace::Access> },
        /// The inner future, polled inside the request span.
        Recording {
            #[pin] future: F,
            span: tracing::Span,
            measured: Measured,
            access: Option<crate::trace::Access>,
        },
    }
}

impl<F, E> Future for RequestSpanFuture<F>
where
    F: Future<Output = Result<Response, E>>,
{
    type Output = Result<Response, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            RequestSpanFutureProj::Disabled {
                future,
                measured,
                access,
            } => {
                let polled = std::task::ready!(future.poll(cx));
                finish_measure(measured, &polled);
                finish_access(access, &polled);
                Poll::Ready(polled)
            }
            RequestSpanFutureProj::Recording {
                future,
                span,
                measured,
                access,
            } => {
                let _entered = span.enter();
                let polled = std::task::ready!(future.poll(cx));
                if let Ok(response) = &polled {
                    crate::trace::record_response(span, response);
                }
                finish_measure(measured, &polled);
                // Inside the span, so an exported log record carries the request's trace.
                finish_access(access, &polled);
                Poll::Ready(polled)
            }
        }
    }
}

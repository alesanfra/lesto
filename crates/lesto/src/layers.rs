//! The tower layers [`App::into_router`](crate::App::into_router) installs, usable on their own.
//!
//! An application that leaves lesto for a plain `axum::Router` (or that mounts a lesto router
//! inside a larger axum one) can keep the behavior it had by adding these:
//!
//! ```
//! use lesto::layers::ProblemLayer;
//!
//! let router: axum::Router = axum::Router::new()
//!     .route("/hello", axum::routing::get(|| async { "hi" }))
//!     .layer(ProblemLayer::new());
//! ```
//!
//! Order matters: `Router::layer` makes the last one added the outermost, and
//! [`ProblemLayer`] belongs outside the panic catcher, so the `500` a caught panic produces
//! gets its `instance` too. It runs *inside* routing, like everything a `Router::layer` adds.
//!
//! Every future here is a concrete type: no `Box::pin` on the happy path.

use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use http::{HeaderValue, Request, header};
use pin_project_lite::pin_project;
use tokio::task::futures::TaskLocalFuture;
use tower_layer::Layer;
use tower_service::Service;

use crate::error::{ErrorFormat, PROBLEM_JSON, Problem, ProblemRendered, RenderContext};

// ---- problems -----------------------------------------------------------------------------

/// Finishes every RFC 9457 response of the requests below it: `instance` is the request path,
/// and the body is written in the [`ErrorFormat`] the application asked for.
///
/// The work happens where the problem is *built* ([`Problem::into_response`]): the layer only
/// publishes the request path and the format, so a problem is serialized once. A
/// `application/problem+json` response built by hand somewhere below — without the
/// [`ProblemRendered`] marker — still takes the slow path, where the body is buffered, parsed
/// and rewritten.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProblemLayer {
    format: ErrorFormat,
}

impl ProblemLayer {
    /// RFC 9457 responses (`application/problem+json`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Render errors in the given format instead.
    pub fn format(format: ErrorFormat) -> Self {
        Self { format }
    }
}

impl<S> Layer<S> for ProblemLayer {
    type Service = ProblemService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ProblemService {
            inner,
            format: self.format,
        }
    }
}

/// The service [`ProblemLayer`] produces.
#[derive(Debug, Clone, Copy)]
pub struct ProblemService<S> {
    inner: S,
    format: ErrorFormat,
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
            format: self.format,
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

/// Slow path: a `problem+json` response somebody else built. Buffer it, parse it, fill in
/// `instance` and rewrite it in the configured format.
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
    let body = match context.format {
        ErrorFormat::Problem => serde_json::to_vec(&problem),
        ErrorFormat::FastApi => {
            parts.headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            serde_json::to_vec(&crate::error::problem_to_fastapi(&problem))
        }
    };
    let body = body.unwrap_or_else(|_| bytes.to_vec());
    let mut response = Response::from_parts(parts, Body::from(body));
    response.extensions_mut().insert(ProblemRendered);
    response
}

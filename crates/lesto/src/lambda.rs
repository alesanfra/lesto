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
//! [`test::invoke`] feeds an event fixture to a router without Lambda, for tests.

use crate::App;
use axum::Router;
use http::Request;
use lambda_http::RequestExt;
use lambda_http::request::RequestContext;
use tower::ServiceExt;
use tower::util::MapRequest;

/// The error type of the Lambda runtime: any `Box<dyn std::error::Error + Send + Sync>`.
pub use lambda_http::Error;

/// How [`serve_with`] behaves inside Lambda.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// Keep the API Gateway stage in the request path (`/prod/notes`). Off by default: the
    /// stage is stripped and routes match as they do locally.
    pub keep_stage: bool,
}

impl Options {
    /// Keep (`true`) or strip (`false`, the default) the API Gateway stage.
    pub fn keep_stage(mut self, keep: bool) -> Self {
        self.keep_stage = keep;
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
    lambda_http::run(with_options(router, options)).await
}

/// The router as Lambda sees it: behind a request mapper that strips the stage unless
/// `keep_stage`.
///
/// The path is rewritten *before* the router sees it (a `Router::layer` would run after
/// routing), and by a mapper rather than through the runtime's
/// `AWS_LAMBDA_HTTP_IGNORE_STAGE_IN_PATH` variable: the option stays local to this router (two
/// routers, two settings, one process) and nothing mutates the environment of a running
/// multi-threaded program.
fn with_options(
    router: Router,
    options: Options,
) -> MapRequest<Router, impl FnMut(lambda_http::Request) -> lambda_http::Request + Clone> {
    router.map_request(move |req: lambda_http::Request| {
        if options.keep_stage {
            req
        } else {
            strip_stage(req)
        }
    })
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
    /// Applies [`Options::default`] (stage stripped); [`invoke_with`] takes explicit options.
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
        let response = with_options(router.clone(), options)
            .oneshot(request)
            .await?;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await?.to_bytes();
        Ok(Response::from_parts(parts, bytes))
    }
}

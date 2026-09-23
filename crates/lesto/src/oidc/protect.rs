//! [`App::protect`](crate::App::protect): a valid token on every route of an app.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use http::header::{self, HeaderValue};
use http::{Request, StatusCode};
use tower_layer::Layer;
use tower_service::Service;

use super::Oidc;
use super::claims::scopes;
use super::extract::verified;
use crate::error::HttpError;

/// A verifier plus the scopes every token must grant: the argument of
/// [`App::protect`](crate::App::protect). An [`Oidc`] converts into one with no scopes;
/// [`Oidc::scopes`] builds one with some.
#[derive(Debug, Clone)]
pub struct Protect {
    pub(crate) oidc: Oidc,
    pub(crate) scopes: Vec<String>,
}

impl Protect {
    /// Require `scopes` too (all of them). A token without one answers `403` with
    /// `WWW-Authenticate: Bearer error="insufficient_scope"`.
    pub fn scopes<I>(mut self, scopes: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        self.scopes.extend(scopes.into_iter().map(Into::into));
        self
    }

    /// The layer that enforces it, for a plain `axum::Router`.
    pub fn layer(&self) -> ProtectLayer {
        ProtectLayer {
            protect: self.clone(),
        }
    }
}

impl From<Oidc> for Protect {
    fn from(oidc: Oidc) -> Self {
        Protect {
            oidc,
            scopes: Vec::new(),
        }
    }
}

/// Refuses a request without a valid token (`401`) or without the required scopes (`403`)
/// before it reaches the handler, and leaves the verifier and the verified claims in the request
/// extensions, so a [`Jwt`](super::Jwt) argument below it verifies nothing twice.
#[derive(Debug, Clone)]
pub struct ProtectLayer {
    protect: Protect,
}

impl<S> Layer<S> for ProtectLayer {
    type Service = ProtectService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ProtectService {
            inner,
            protect: self.protect.clone(),
        }
    }
}

/// The service [`ProtectLayer`] produces.
#[derive(Debug, Clone)]
pub struct ProtectService<S> {
    inner: S,
    protect: Protect,
}

impl<S> Service<Request<Body>> for ProtectService<S>
where
    S: Service<Request<Body>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        // The clone that was polled ready is the one that serves the request.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let protect = self.protect.clone();
        Box::pin(async move {
            let (mut parts, body) = request.into_parts();
            let claims = match verified(&mut parts, Some(&protect.oidc)).await {
                Ok(claims) => claims,
                Err(rejection) => return Ok(rejection.into_response()),
            };
            let granted: Vec<&str> = scopes(&claims).collect();
            if !protect.scopes.iter().all(|s| granted.contains(&s.as_str())) {
                return Ok(insufficient_scope(&protect.scopes).into_response());
            }
            parts.extensions.insert(protect.oidc);
            inner.call(Request::from_parts(parts, body)).await
        })
    }
}

/// `403` with the RFC 6750 challenge naming the scopes the operation needs.
fn insufficient_scope(required: &[String]) -> HttpError {
    let required = required.join(" ");
    let challenge = HeaderValue::from_str(&format!(
        "Bearer error=\"insufficient_scope\", scope=\"{required}\""
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("Bearer error=\"insufficient_scope\""));
    HttpError::new(
        StatusCode::FORBIDDEN,
        format!("This operation needs the scopes: {required}"),
    )
    .with_header(header::WWW_AUTHENTICATE, challenge)
}

//! Route metadata and the `RouteSet` collection that ties handlers to their documentation.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pin_project_lite::pin_project;

use axum::handler::Handler;
use axum::response::Response;
use axum::routing::{MethodFilter, MethodRouter};
use http::{Method, StatusCode};
use schemars::SchemaGenerator;

use indexmap::IndexMap;

use crate::error::IntoStatus;
use crate::openapi::{self, Operation, SecurityScheme};
use crate::operation::{
    OperationBuilder, OperationHandler, OperationInput, OperationOutput, reason_phrase,
};

/// Everything about a route that is not derivable from the handler's types.
#[derive(Debug, Clone)]
pub struct RouteMeta {
    pub method: Method,
    pub path: String,
    /// Name of the handler function, set by the route attribute; the default `operationId`
    /// starts with it.
    pub name: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub operation_id: Option<String>,
    pub tags: Vec<String>,
    /// Success status, 200 by default. Applied to the live response as well as the docs.
    pub status: u16,
    pub deprecated: bool,
    /// Extra error statuses to document with the `HttpError` body.
    pub error_statuses: Vec<u16>,
    /// Security requirements declared on the route (scheme name, scopes). Alternatives.
    pub security: Vec<(String, Vec<String>)>,
    /// Explicitly public: emits `security: []`, overriding app-wide requirements.
    pub public: bool,
}

impl RouteMeta {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            name: None,
            summary: None,
            description: None,
            operation_id: None,
            tags: Vec::new(),
            status: 200,
            deprecated: false,
            error_statuses: Vec::new(),
            security: Vec::new(),
            public: false,
        }
    }

    /// The handler's name, used to build the default `operationId`.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn operation_id(mut self, id: impl Into<String>) -> Self {
        self.operation_id = Some(id.into());
        self
    }

    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn status(mut self, status: impl IntoStatus) -> Self {
        self.status = status.into_status().as_u16();
        self
    }

    pub fn deprecated(mut self, deprecated: bool) -> Self {
        self.deprecated = deprecated;
        self
    }

    /// Document that the handler may answer with this error status (`HttpError` body).
    pub fn error(mut self, status: impl IntoStatus) -> Self {
        self.error_statuses.push(status.into_status().as_u16());
        self
    }

    /// Require security scheme `name` (registered with [`crate::App::security_scheme`] or by an
    /// extractor elsewhere) with `scopes`. Several calls are alternatives.
    pub fn security(mut self, name: impl Into<String>, scopes: &[&str]) -> Self {
        self.security
            .push((name.into(), scopes.iter().map(|s| s.to_string()).collect()));
        self
    }

    /// Mark the route as public even when the app declares default security.
    pub fn public(mut self) -> Self {
        self.public = true;
        self
    }

    /// `operation_id`, or FastAPI's default: `{name}_{path}_{method}` with every non-word
    /// character replaced by `_` (`get_user_users__id__get`). `path` is the final path, so two
    /// handlers with the same name in different modules or nested apps get different ids.
    fn default_operation_id(&self, path: &str) -> String {
        if let Some(id) = &self.operation_id {
            return id.clone();
        }
        let mut id = String::new();
        if let Some(name) = &self.name {
            id.push_str(name);
        }
        for c in path.chars() {
            id.push(if c.is_ascii_alphanumeric() { c } else { '_' });
        }
        if id.starts_with('_') && self.name.is_none() {
            id.remove(0);
        }
        id.push('_');
        id.push_str(&self.method.as_str().to_ascii_lowercase());
        id
    }

    fn base_operation(&self, path: &str) -> Operation {
        Operation {
            tags: self.tags.clone(),
            summary: self.summary.clone(),
            description: self.description.clone(),
            operation_id: Some(self.default_operation_id(path)),
            deprecated: self.deprecated,
            ..Operation::default()
        }
    }
}

macro_rules! method_ctor {
    ($($name:ident => $method:ident),* $(,)?) => {
        $(
            #[doc = concat!("Metadata for a `", stringify!($method), "` route at `path`.")]
            pub fn $name(path: impl Into<String>) -> RouteMeta {
                RouteMeta::new(Method::$method, path)
            }
        )*
    };
}

method_ctor! {
    get => GET, post => POST, put => PUT, patch => PATCH, delete => DELETE, head => HEAD, options => OPTIONS,
}

/// Implemented by the `#[lesto::get(...)]` family of macros on a marker type that shares
/// the handler's name, so `routes![handler]` can find both the function and its metadata.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a lesto route: the handler is missing its route attribute",
    label = "no `#[lesto::get(\"/path\")]` (or post/put/patch/delete) on this function",
    note = "annotate the handler, e.g. `#[lesto::get(\"/items/{{id}}\")] async fn {Self}(...)`, then register it with `routes![{Self}]`",
    note = "if the handler lives in another module it must be `pub`, and `routes!` needs its path: `routes![users::list]`"
)]
pub trait RouteInfo {
    fn meta() -> RouteMeta;
}

/// Describes a handler's inputs and outputs. Re-runnable so the document can be rebuilt.
pub type DescribeFn = Arc<dyn Fn(&mut OperationBuilder<'_>) + Send + Sync>;

/// A route's metadata plus the function that documents its handler.
#[derive(Clone)]
pub struct PendingOperation {
    pub meta: RouteMeta,
    pub describe: DescribeFn,
}

impl PendingOperation {
    /// Build the OpenAPI operation. `path` is the final OpenAPI path (prefixes applied).
    pub fn operation(
        &self,
        path: &str,
        generator: &mut SchemaGenerator,
        security_schemes: &mut IndexMap<String, SecurityScheme>,
    ) -> Operation {
        let mut operation = self.meta.base_operation(path);
        {
            let mut builder = OperationBuilder {
                operation: &mut operation,
                generator,
                path,
                method: &self.meta.method,
                security_schemes,
            };
            (self.describe)(&mut builder);
            for (name, scopes) in &self.meta.security {
                let scopes: Vec<&str> = scopes.iter().map(String::as_str).collect();
                builder.security_requirement(name, &scopes);
            }
            for status in &self.meta.error_statuses {
                builder.error_response(*status, &reason_phrase(*status));
            }
        }
        if self.meta.public {
            operation.security = Some(Vec::new());
        }
        // Success codes first, then errors, `default` last.
        operation
            .responses
            .sort_by(|a, _, b, _| match (a.parse::<u16>(), b.parse::<u16>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                (Err(_), Err(_)) => a.cmp(b),
            });
        if operation.responses.is_empty() {
            operation.responses.insert(
                self.meta.status.to_string(),
                openapi::Response {
                    description: reason_phrase(self.meta.status),
                    content: Default::default(),
                },
            );
        }
        operation
    }
}

pub(crate) struct RouteEntry<S> {
    pub pending: PendingOperation,
    pub method_router: MethodRouter<S>,
}

/// A group of routes with the state type `S` still to be inferred.
///
/// Built by [`routes!`](crate::routes) or by hand with [`RouteSet::add`].
pub struct RouteSet<S = ()> {
    pub(crate) entries: Vec<RouteEntry<S>>,
}

impl<S> Default for RouteSet<S> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<S> RouteSet<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `handler` at `meta`, deriving its documentation from the argument tuple `I`
    /// and the return type `O`.
    pub fn add<H, I, O, T>(mut self, meta: RouteMeta, handler: H) -> Self
    where
        H: Handler<T, S> + OperationHandler<I, O>,
        I: OperationInput + 'static,
        O: OperationOutput + 'static,
        T: 'static,
    {
        let filter = MethodFilter::try_from(meta.method.clone())
            .unwrap_or_else(|_| panic!("unsupported HTTP method {}", meta.method));
        let status = meta.status;
        let method_router = if status == 200 {
            axum::routing::on(filter, handler)
        } else {
            axum::routing::on(
                filter,
                WithStatus {
                    inner: handler,
                    status: StatusCode::from_u16(status).expect("valid status"),
                },
            )
        };
        let describe: DescribeFn = Arc::new(move |b| {
            I::describe(b);
            O::describe(b, status);
        });
        self.entries.push(RouteEntry {
            pending: PendingOperation { meta, describe },
            method_router,
        });
        self
    }

    /// Merge another set into this one.
    pub fn extend(mut self, other: RouteSet<S>) -> Self {
        self.entries.extend(other.entries);
        self
    }
}

/// Rewrites a handler's `200 OK` into the route's declared success status.
#[derive(Clone)]
struct WithStatus<H> {
    inner: H,
    status: StatusCode,
}

impl<H, T, S> Handler<T, S> for WithStatus<H>
where
    H: Handler<T, S>,
{
    type Future = WithStatusFuture<H::Future>;

    fn call(self, req: axum::extract::Request, state: S) -> Self::Future {
        WithStatusFuture {
            inner: self.inner.call(req, state),
            status: self.status,
        }
    }
}

pin_project! {
    /// The future of [`WithStatus`]: the handler's own, with the status rewritten on the way
    /// out. Concrete, so a `status = N` route allocates no more than any other.
    pub struct WithStatusFuture<F> {
        #[pin]
        inner: F,
        status: StatusCode,
    }
}

impl<F> Future for WithStatusFuture<F>
where
    F: Future<Output = Response>,
{
    type Output = Response;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Response> {
        let this = self.project();
        let mut response = std::task::ready!(this.inner.poll(cx));
        if response.status() == StatusCode::OK {
            *response.status_mut() = *this.status;
        }
        std::task::Poll::Ready(response)
    }
}

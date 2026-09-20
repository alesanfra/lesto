//! `App`: an `axum::Router` that also accumulates an OpenAPI document.

use std::convert::Infallible;

use axum::body::Bytes;
use axum::extract::Request;
use axum::response::IntoResponse;
use axum::routing::{Router, get};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use schemars::SchemaGenerator;
use schemars::generate::SchemaSettings;
use schemars::transform::ReplaceBoolSchemas;
use tower_layer::Layer;
use tower_service::Service;

use crate::docs::{self, DocsAssets};
use crate::error::HttpError;
use crate::openapi::{self, Components, OpenApi, SecurityRequirement, SecurityScheme, Tag};
use crate::operation::{OperationHandler, OperationInput, OperationOutput};
use crate::route::{PendingOperation, RouteMeta, RouteSet};
use crate::trace::Trace;

/// The application: routes plus their OpenAPI description.
///
/// Mirrors `axum::Router<S>`: build with a state type `S`, then call
/// [`with_state`](App::with_state) to obtain an `App<()>` that can be served.
pub struct App<S = ()> {
    router: Router<S>,
    spec: OpenApi,
    /// (final OpenAPI path, operation) pairs, documented lazily so configuration order does not matter.
    operations: Vec<(String, PendingOperation)>,
    security_schemes: indexmap::IndexMap<String, SecurityScheme>,
    openapi_url: Option<String>,
    docs_url: Option<String>,
    swagger_url: Option<String>,
    docs_assets: DocsAssets,
    trace: Trace,
    /// A fallback was installed through [`App::fallback`]: keep it instead of the problem 404.
    custom_fallback: bool,
}

fn schema_generator() -> SchemaGenerator {
    SchemaSettings::draft2020_12()
        .with(|s| {
            s.definitions_path = "/components/schemas".into();
            s.meta_schema = None;
        })
        // Swagger UI cannot render boolean schemas (`true` for `serde_json::Value`, `false`),
        // so emit `{}` / `{"not": {}}` instead. `additionalProperties: true|false` is fine.
        .with_transform(bool_schema_transform())
        .into_generator()
}

fn bool_schema_transform() -> ReplaceBoolSchemas {
    let mut transform = ReplaceBoolSchemas::default();
    transform.skip_additional_properties = true;
    transform
}

impl<S> Default for App<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<S> App<S>
where
    S: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            spec: OpenApi::default(),
            operations: Vec::new(),
            security_schemes: indexmap::IndexMap::new(),
            openapi_url: Some("/openapi.json".to_string()),
            docs_url: Some("/docs".to_string()),
            swagger_url: Some("/swagger".to_string()),
            docs_assets: DocsAssets::default(),
            trace: Trace::default(),
            custom_fallback: false,
        }
    }

    // ---- metadata ---------------------------------------------------------------------

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.spec.info.title = title.into();
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.spec.info.version = version.into();
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.spec.info.description = Some(description.into());
        self
    }

    /// Describe a tag used by routes.
    pub fn tag(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        self.spec.tags.push(Tag {
            name: name.into(),
            description: Some(description.into()),
        });
        self
    }

    pub fn server(mut self, url: impl Into<String>) -> Self {
        self.spec.servers.push(openapi::Server {
            url: url.into(),
            description: None,
        });
        self
    }

    /// Register a security scheme under `components.securitySchemes`.
    ///
    /// Extractors from [`crate::security`] register theirs automatically; use this for schemes
    /// referenced only by [`RouteMeta::security`] / the `security(..)` macro option, or to
    /// customize a built-in one (e.g. `bearer_jwt()` under the name `bearerAuth`).
    pub fn security_scheme(mut self, name: impl Into<String>, scheme: SecurityScheme) -> Self {
        self.security_schemes.insert(name.into(), scheme);
        self
    }

    /// Require `name` (with `scopes`) on every operation that does not declare its own
    /// security. Call several times for alternatives; mark exceptions with `RouteMeta::public`.
    pub fn security(mut self, name: impl Into<String>, scopes: &[&str]) -> Self {
        let mut requirement = SecurityRequirement::new();
        requirement.insert(name.into(), scopes.iter().map(|s| s.to_string()).collect());
        self.spec
            .security
            .get_or_insert_with(Vec::new)
            .push(requirement);
        self
    }

    /// Where to serve the OpenAPI JSON (`/openapi.json` by default, `None` disables it and
    /// therefore also the documentation pages).
    pub fn openapi_url(mut self, url: Option<&str>) -> Self {
        self.openapi_url = url.map(String::from);
        self
    }

    /// Where to serve the Scalar API Reference (`/docs` by default, `None` disables it).
    pub fn docs_url(mut self, url: Option<&str>) -> Self {
        self.docs_url = url.map(String::from);
        self
    }

    /// Where to serve Swagger UI (`/swagger` by default, `None` disables it).
    pub fn swagger_url(mut self, url: Option<&str>) -> Self {
        self.swagger_url = url.map(String::from);
        self
    }

    /// URL of the Scalar script the `/docs` page loads, for self-hosted or mirrored assets.
    ///
    /// By default a pinned version on jsDelivr with a subresource integrity hash; a custom URL
    /// is used as given, with no integrity attribute.
    pub fn scalar_script_url(mut self, url: impl Into<String>) -> Self {
        self.docs_assets.scalar_script = Some(url.into());
        self
    }

    /// Directory the `/swagger` page loads `swagger-ui.css` and `swagger-ui-bundle.js` from
    /// (the contents of the `swagger-ui-dist` package), for self-hosted or mirrored assets.
    ///
    /// By default a pinned version on jsDelivr with subresource integrity hashes; a custom URL
    /// is used as given, with no integrity attributes.
    pub fn swagger_ui_base_url(mut self, url: impl Into<String>) -> Self {
        self.docs_assets.swagger_ui_base = Some(url.into());
        self
    }

    /// What the per-request span records, or [`Trace::off()`] for no span at all.
    ///
    /// Every request runs inside a `tracing` span whose fields are the OpenTelemetry semantic
    /// conventions for HTTP servers; see [`crate::trace`].
    pub fn trace(mut self, trace: Trace) -> Self {
        self.trace = trace;
        self
    }

    // ---- routes -----------------------------------------------------------------------

    /// Register a set of routes, typically built with [`routes!`](crate::routes).
    pub fn routes(mut self, set: RouteSet<S>) -> Self {
        for entry in set.entries {
            let path = openapi::openapi_path(&entry.pending.meta.path);
            self.router = self
                .router
                .route(&entry.pending.meta.path, entry.method_router);
            self.operations.push((path, entry.pending));
        }
        self
    }

    /// Register a single handler with explicit metadata, e.g. `app.route(lesto::get("/x"), handler)`.
    pub fn route<H, I, O, T>(self, meta: RouteMeta, handler: H) -> Self
    where
        H: axum::handler::Handler<T, S> + OperationHandler<I, O>,
        I: OperationInput + 'static,
        O: OperationOutput + 'static,
        T: 'static,
    {
        self.routes(RouteSet::new().add(meta, handler))
    }

    /// Handler for requests that match no route, replacing the default `404` problem.
    ///
    /// Not documented: a fallback is not an operation.
    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        H: axum::handler::Handler<T, S>,
        T: 'static,
    {
        self.router = self.router.fallback(handler);
        self.custom_fallback = true;
        self
    }

    /// Mount another `App` under `prefix`, merging its documentation.
    ///
    /// Equivalent to FastAPI's `include_router(router, prefix=...)`. Documentation pages, the
    /// error format, the fallback and app-wide security of the nested app are ignored; only the
    /// root app's apply.
    pub fn nest(mut self, prefix: &str, other: App<S>) -> Self {
        let prefix = prefix.trim_end_matches('/');
        let App {
            router,
            spec,
            operations,
            security_schemes,
            ..
        } = other;
        for (path, pending) in operations {
            // axum mounts a nested `/` at exactly `prefix`, so document it the same way.
            let full = if path == "/" {
                prefix.to_string()
            } else {
                format!("{prefix}{path}")
            };
            self.operations.push((full, pending));
        }
        for (name, scheme) in security_schemes {
            self.security_schemes.entry(name).or_insert(scheme);
        }
        for tag in spec.tags {
            if !self.spec.tags.iter().any(|t| t.name == tag.name) {
                self.spec.tags.push(tag);
            }
        }
        self.router = self.router.nest(prefix, router);
        self
    }

    /// Apply a tower layer (middleware) to every route registered so far.
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: Service<Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<Request>>::Response: IntoResponse + 'static,
        <L::Service as Service<Request>>::Error: Into<Infallible> + 'static,
        <L::Service as Service<Request>>::Future: Send + 'static,
    {
        self.router = self.router.layer(layer);
        self
    }

    /// Escape hatch: transform the underlying `axum::Router` directly.
    ///
    /// Routes added this way are not documented.
    pub fn map_router(mut self, f: impl FnOnce(Router<S>) -> Router<S>) -> Self {
        self.router = f(self.router);
        self
    }

    /// Provide the state, producing an `App<()>` that can be served.
    pub fn with_state<S2>(self, state: S) -> App<S2> {
        App {
            router: self.router.with_state(state),
            spec: self.spec,
            operations: self.operations,
            security_schemes: self.security_schemes,
            openapi_url: self.openapi_url,
            docs_url: self.docs_url,
            swagger_url: self.swagger_url,
            docs_assets: self.docs_assets,
            trace: self.trace,
            custom_fallback: self.custom_fallback,
        }
    }

    // ---- output -----------------------------------------------------------------------

    /// The OpenAPI document for everything registered so far.
    pub fn openapi(&self) -> OpenApi {
        let mut spec = self.spec.clone();
        let mut generator = schema_generator();
        let mut security_schemes = self.security_schemes.clone();
        for (path, pending) in &self.operations {
            let operation = pending.operation(path, &mut generator, &mut security_schemes);
            let item = spec.paths.entry(path.clone()).or_default();
            if let Some(slot) = item.slot_mut(&pending.meta.method) {
                *slot = Some(operation);
            }
        }
        let schemas = generator.take_definitions(true);
        if !schemas.is_empty() || !security_schemes.is_empty() {
            let mut sorted: Vec<_> = schemas.into_iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            security_schemes.sort_keys();
            spec.components = Some(Components {
                schemas: sorted
                    .into_iter()
                    .filter_map(|(k, v)| schemars::Schema::try_from(v).ok().map(|s| (k, s)))
                    .collect(),
                security_schemes,
            });
        }
        spec
    }

    /// The JSON body of the OpenAPI document, handy for tests and codegen.
    pub fn openapi_json(&self) -> String {
        serde_json::to_string_pretty(&self.openapi()).expect("OpenAPI document is serializable")
    }

    /// Finish building: mount the documentation routes, install the problem fallbacks and the
    /// error middleware, and return the `axum::Router`.
    ///
    /// Every error the router produces on its own is RFC 9457: unknown routes answer a `404`
    /// problem (unless [`App::fallback`] was called), a known route with the wrong method a
    /// `405`, and a handler that panics a `500` with the panic message kept out of the response.
    pub fn into_router(self) -> Router<S> {
        let spec = self.openapi_url.as_ref().map(|_| self.openapi());
        let mut router = self.router;

        if let (Some(openapi_url), Some(spec)) = (self.openapi_url.clone(), spec) {
            let json = serde_json::to_string(&spec).expect("OpenAPI document is serializable");
            let title = spec.info.title.clone();
            router = router.route(&openapi_url, served_once(json, "application/json"));
            if let Some(docs_url) = &self.docs_url {
                let html = docs::scalar_html(
                    &docs::relative_url(docs_url, &openapi_url),
                    &title,
                    &self.docs_assets,
                );
                router = router.route(docs_url, served_once(html, HTML));
            }
            if let Some(swagger_url) = &self.swagger_url {
                let html = docs::swagger_ui_html(
                    &docs::relative_url(swagger_url, &openapi_url),
                    &title,
                    &self.docs_assets,
                );
                router = router.route(swagger_url, served_once(html, HTML));
            }
        }

        if !self.custom_fallback {
            router =
                router.fallback(|| async { HttpError::new(StatusCode::NOT_FOUND, "Not Found") });
        }
        router = router.method_not_allowed_fallback(|| async {
            HttpError::new(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
        });

        // One `Router::layer` call, not three: axum re-boxes every route (and its future) on
        // each one, so the layers are stacked first and added together.
        let layers = tower_layer::Stack::new(
            tower_layer::Stack::new(crate::layers::CatchPanicLayer, crate::layers::ProblemLayer),
            // Outermost, so the span covers the fallbacks, the panic catcher and the rendering
            // of the problem body — everything the client waits for. It still runs *inside*
            // routing, which is what makes `MatchedPath` (and so `http.route`) available.
            crate::layers::RequestSpanLayer::with(self.trace),
        );
        router.layer(layers)
    }
}

impl App<()> {
    /// Serve on the address the environment says (see [`bind_address`]) or on a socket
    /// inherited from the parent process (`lesto dev`, systemd socket activation, `systemfd`;
    /// see [`listener`]), until `SIGTERM` or `Ctrl-C`.
    ///
    /// Shutdown is graceful: the listener closes at once, requests already in flight run to
    /// completion, then the future resolves. Pair it with your platform's termination grace
    /// period (Kubernetes gives 30 s by default).
    ///
    /// With the `otel` feature, this is also where telemetry is set up: if
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` is set and no `tracing` subscriber has been installed,
    /// spans go to the console and to the collector, and are flushed before this returns. See
    /// [`crate::otel`].
    pub async fn serve(self) -> std::io::Result<()> {
        let addr = bind_address()?;
        self.serve_at(addr).await
    }

    /// [`serve`](Self::serve) on an explicit address, ignoring `LESTO_HOST`/`LESTO_PORT`. An
    /// inherited socket still wins.
    pub async fn serve_at(self, addr: impl tokio::net::ToSocketAddrs) -> std::io::Result<()> {
        let listener = listener(addr).await?;
        self.serve_on(listener).await
    }

    /// [`serve`](Self::serve) on a listener you bound yourself (a fixed port in tests, TLS
    /// termination in front, a Unix-socket proxy that speaks TCP to the app).
    pub async fn serve_on(self, listener: tokio::net::TcpListener) -> std::io::Result<()> {
        self.serve_until(listener, shutdown_signal()).await
    }

    /// [`serve_on`](Self::serve_on) with your own shutdown trigger instead of the process
    /// signals: the server stops accepting connections when `shutdown` resolves and returns
    /// once in-flight requests are done.
    pub async fn serve_until(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        // Held until the server is done, so the last spans are flushed after the in-flight
        // requests finish. Does nothing unless the `otel` feature is on, the environment names
        // a collector, and the application installed no subscriber of its own.
        #[cfg(feature = "otel")]
        let _telemetry = crate::otel::auto_init(&self.spec.info.title);
        axum::serve(listener, self.into_router())
            .with_graceful_shutdown(shutdown)
            .await
    }
}

/// Resolves on `Ctrl-C` (`SIGINT`) or, on Unix, `SIGTERM`: what an orchestrator sends first.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// The address [`App::serve`] binds: `LESTO_HOST` (default `127.0.0.1`) and `LESTO_PORT`
/// (default `8000`). `PORT` is honored when `LESTO_PORT` is absent, for platforms that set it
/// (Heroku, Cloud Run). A port that is not a number is an `InvalidInput` error.
pub fn bind_address() -> std::io::Result<(String, u16)> {
    let host = std::env::var("LESTO_HOST")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = match std::env::var("LESTO_PORT").or_else(|_| std::env::var("PORT")) {
        Ok(raw) => raw.trim().parse::<u16>().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("LESTO_PORT/PORT must be a port number, got `{raw}`"),
            )
        })?,
        Err(_) => 8000,
    };
    Ok((host, port))
}

/// The listener [`App::serve`] uses: an inherited socket when there is one, else a fresh bind.
///
/// The inherited socket follows systemd's protocol: `LISTEN_FDS=1` and, optionally,
/// `LISTEN_PID` equal to the current process id, with the socket on file descriptor 3.
/// `lesto dev` uses it to keep the port open while the application rebuilds. The socket is
/// taken at most once per process (a second call binds `addr`); the environment variables are
/// left untouched, since changing the environment of a running multi-threaded process is
/// unsound. Unix only; elsewhere the variables are ignored.
pub async fn listener(
    addr: impl tokio::net::ToSocketAddrs,
) -> std::io::Result<tokio::net::TcpListener> {
    if let Some(inherited) = inherited_listener()? {
        return Ok(inherited);
    }
    tokio::net::TcpListener::bind(addr).await
}

#[cfg(unix)]
fn inherited_listener() -> std::io::Result<Option<tokio::net::TcpListener>> {
    use std::os::fd::FromRawFd;
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKEN: AtomicBool = AtomicBool::new(false);

    let fds: u32 = match std::env::var("LISTEN_FDS") {
        Ok(v) => v.trim().parse().unwrap_or(0),
        Err(_) => 0,
    };
    if fds == 0 {
        return Ok(None);
    }
    if let Ok(pid) = std::env::var("LISTEN_PID")
        && pid.trim() != std::process::id().to_string()
    {
        return Ok(None);
    }
    if TAKEN.swap(true, Ordering::SeqCst) {
        return Ok(None);
    }
    // SAFETY: fd 3 is the first inherited socket under the LISTEN_FDS protocol, and `TAKEN`
    // guarantees this process takes ownership of it exactly once.
    let std_listener = unsafe { std::net::TcpListener::from_raw_fd(3) };
    std_listener.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(std_listener).map(Some)
}

#[cfg(not(unix))]
fn inherited_listener() -> std::io::Result<Option<tokio::net::TcpListener>> {
    Ok(None)
}

/// `text/html`, as the documentation pages are served.
const HTML: &str = "text/html; charset=utf-8";

/// A route that answers the same bytes to every request: the OpenAPI document and the two
/// documentation pages.
///
/// The body is built once and every response is a reference-count increment on it, never a
/// copy. It carries an `ETag` (a hash of those bytes, computed once too), so a browser that
/// comes back with `If-None-Match` gets a `304` and no body at all.
fn served_once<S>(body: String, content_type: &'static str) -> axum::routing::MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    let etag = HeaderValue::from_str(&format!("\"{:016x}\"", hasher.finish()))
        .expect("a hexadecimal ETag is a valid header value");
    let content_type = HeaderValue::from_static(content_type);
    let body = Bytes::from(body);

    get(move |headers: HeaderMap| {
        let (body, etag, content_type) = (body.clone(), etag.clone(), content_type.clone());
        async move {
            if headers
                .get_all(header::IF_NONE_MATCH)
                .iter()
                .any(|candidate| candidate == etag)
            {
                return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
            }
            (
                [(header::CONTENT_TYPE, content_type), (header::ETAG, etag)],
                body,
            )
                .into_response()
        }
    })
}

impl<S> From<App<S>> for Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn from(app: App<S>) -> Self {
        app.into_router()
    }
}

#[cfg(test)]
mod tests {
    use super::bind_address;

    #[test]
    fn bind_address_from_env() {
        // One test, sequential: environment variables are process-wide.
        // SAFETY: this is the only test in the binary that touches the environment, and the
        // unit-test harness runs it on a thread of its own with no other reader.
        unsafe {
            for v in ["LESTO_HOST", "LESTO_PORT", "PORT"] {
                std::env::remove_var(v);
            }
            assert_eq!(bind_address().unwrap(), ("127.0.0.1".to_string(), 8000));
            std::env::set_var("PORT", "9000");
            assert_eq!(bind_address().unwrap().1, 9000, "PORT is the fallback");
            std::env::set_var("LESTO_PORT", "8765");
            std::env::set_var("LESTO_HOST", "0.0.0.0");
            assert_eq!(bind_address().unwrap(), ("0.0.0.0".to_string(), 8765));
            std::env::set_var("LESTO_PORT", "nope");
            let err = bind_address().unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
            for v in ["LESTO_HOST", "LESTO_PORT", "PORT"] {
                std::env::remove_var(v);
            }
        }
    }
}

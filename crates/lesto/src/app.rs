//! `App`: an `axum::Router` that also accumulates an OpenAPI document.

use std::convert::Infallible;
use std::time::Duration;

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
    /// How long in-flight requests may run after the shutdown signal.
    shutdown_timeout: Option<Duration>,
    /// Per-request time limit ([`App::timeout`]).
    timeout: Option<Duration>,
    /// Request body limit ([`App::body_limit`]); `None` keeps axum's 2 MB default.
    body_limit: Option<axum::extract::DefaultBodyLimit>,
    /// [`App::cors`].
    cors: Option<tower_http::cors::CorsLayer>,
    /// [`App::compression`].
    #[cfg(feature = "compression")]
    compression: bool,
    /// [`App::request_id`].
    request_id: bool,
    /// [`App::oidc`]: the verifier `Jwt` arguments find in the request extensions.
    #[cfg(feature = "oidc")]
    oidc: Option<crate::oidc::Oidc>,
    /// [`App::protect`]: applied to this app's routes when it is nested or served.
    #[cfg(feature = "oidc")]
    protect: Option<crate::oidc::Protect>,
    /// [`App::mcp`]: the MCP endpoint, added when the router is built.
    #[cfg(feature = "mcp")]
    mcp: Option<crate::mcp::Mcp>,
}

/// Default of [`App::shutdown_timeout`]: Kubernetes' own termination grace period.
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// An empty application: no routes, title "API", version "0.1.0", docs at `/docs`.
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
            shutdown_timeout: Some(DEFAULT_SHUTDOWN_TIMEOUT),
            timeout: None,
            body_limit: None,
            cors: None,
            #[cfg(feature = "compression")]
            compression: false,
            request_id: false,
            #[cfg(feature = "oidc")]
            oidc: None,
            #[cfg(feature = "oidc")]
            protect: None,
            #[cfg(feature = "mcp")]
            mcp: None,
        }
    }

    // ---- metadata ---------------------------------------------------------------------

    /// `info.title` of the OpenAPI document; also the title of the docs pages.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.spec.info.title = title.into();
        self
    }

    /// `info.version` of the OpenAPI document (the version of your API, not of OpenAPI).
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.spec.info.version = version.into();
        self
    }

    /// `info.description` of the OpenAPI document (CommonMark).
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

    /// Add an entry to `servers`: a base URL clients should call.
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
    ///
    /// [`Trace::off()`] is about what is recorded, not about speed: the layer is installed
    /// either way, and with no interested subscriber the span already costs a branch.
    pub fn trace(mut self, trace: Trace) -> Self {
        self.trace = trace;
        self
    }

    /// How long requests still in flight may run once shutdown starts: 30 s by default
    /// ([`DEFAULT_SHUTDOWN_TIMEOUT`]), `None` to wait for them however long they take.
    ///
    /// When the deadline passes, the serve future resolves anyway (with a `warn` log), so one
    /// hung handler cannot keep the process alive until the orchestrator's `SIGKILL`. Keep it
    /// below your platform's grace period (Kubernetes: `terminationGracePeriodSeconds`, 30 s).
    pub fn shutdown_timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.shutdown_timeout = timeout.into();
        self
    }

    /// Answer `503 Service Unavailable` (a problem) to any request that runs longer than
    /// `limit`, and drop its handler. Off by default.
    ///
    /// Covers the whole request below routing, body extraction included, so a client that
    /// trickles its body in is cut off too. Costs nothing when not set; see
    /// [`layers::TimeoutLayer`](crate::layers::TimeoutLayer) to use it on a plain router.
    pub fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }

    /// Largest request body the extractors accept, in bytes; `None` for no limit. Without a
    /// call, axum's default applies: **2 MB**. A larger body answers `413 Payload Too Large`
    /// as a problem.
    pub fn body_limit(mut self, bytes: impl Into<Option<usize>>) -> Self {
        self.body_limit = Some(match bytes.into() {
            Some(bytes) => axum::extract::DefaultBodyLimit::max(bytes),
            None => axum::extract::DefaultBodyLimit::disable(),
        });
        self
    }

    /// Answer CORS preflights and add the CORS headers, with tower-http's `CorsLayer`
    /// (re-exported as [`lesto::cors`](crate::cors)):
    ///
    /// ```ignore
    /// use lesto::cors::{Any, CorsLayer};
    /// app.cors(CorsLayer::new().allow_origin(["https://app.example".parse()?]).allow_headers(Any))
    /// ```
    ///
    /// Installed outside every other layer, so error responses (a `404` problem, a caught
    /// panic) carry the headers too and a browser can read them.
    pub fn cors(mut self, cors: tower_http::cors::CorsLayer) -> Self {
        self.cors = Some(cors);
        self
    }

    /// gzip response bodies for clients that send `Accept-Encoding: gzip` (tower-http's
    /// `CompressionLayer`; feature `compression`, on by default). Small bodies and already
    /// compressed media types are left alone.
    #[cfg(feature = "compression")]
    pub fn compression(mut self) -> Self {
        self.compression = true;
        self
    }

    /// Give every request an `x-request-id` (a UUID v4 unless the client or a proxy sent one)
    /// and copy it to the response, so a log line, a trace and a client report can be matched.
    pub fn request_id(mut self) -> Self {
        self.request_id = true;
        self
    }

    /// Serve the routes marked `mcp = "tool"`, `"resource"` or `"prompt"` to agents over the
    /// Model Context Protocol, at `/mcp` unless [`Mcp::path`](crate::mcp::Mcp::path) says
    /// otherwise. See [`crate::mcp`].
    ///
    /// Only the app that is served (or turned into a router) serves MCP: the routes of nested
    /// apps are included, their own `mcp` configuration is ignored. The endpoint sits behind
    /// every layer of the app, [`App::protect`] included, and is not part of the OpenAPI
    /// document.
    #[cfg(feature = "mcp")]
    pub fn mcp(mut self, mcp: crate::mcp::Mcp) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// Verify the bearer tokens of [`Jwt`](crate::oidc::Jwt) arguments with `oidc`, and register
    /// the `openIdConnect` security scheme (Scalar and Swagger UI then offer to log in).
    ///
    /// Covers every route the app serves, nested apps included. Feature `oidc`.
    #[cfg(feature = "oidc")]
    pub fn oidc(mut self, oidc: crate::oidc::Oidc) -> Self {
        self.security_schemes
            .insert(crate::oidc::SCHEME_NAME.to_string(), oidc.security_scheme());
        self.oidc = Some(oidc);
        self
    }

    /// Require a valid token on every route of this app, whatever the handlers' arguments, and
    /// document the `openIdConnect` requirement on each of them. Typically called on an app that
    /// is then [`nest`](Self::nest)ed, so the routes next to it stay public:
    ///
    /// ```ignore
    /// let admin = App::new().routes(routes![stats, purge]).protect(auth.clone().scopes(["admin"]));
    /// App::new().oidc(auth).routes(routes![health]).nest("/admin", admin)
    /// ```
    ///
    /// No token or an invalid one answers `401`; a token without one of the required scopes
    /// answers `403`. A [`Jwt`](crate::oidc::Jwt) argument in a protected route reads the claims
    /// already verified. The documentation routes of the app that is served are never
    /// protected. Every route of the app is, including one marked `public`. Feature `oidc`.
    #[cfg(feature = "oidc")]
    pub fn protect(mut self, protect: impl Into<crate::oidc::Protect>) -> Self {
        let protect = protect.into();
        self.security_schemes
            .entry(crate::oidc::SCHEME_NAME.to_string())
            .or_insert_with(|| protect.oidc.security_scheme());
        self.protect = Some(protect);
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
    /// fallback and app-wide security of the nested app are ignored; only the root app's apply.
    pub fn nest(mut self, prefix: &str, other: App<S>) -> Self {
        let prefix = prefix.trim_end_matches('/');
        #[cfg(feature = "oidc")]
        let other = {
            let mut other = other.apply_protection();
            if self.oidc.is_none() {
                self.oidc = other.oidc.take();
            }
            other
        };
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

    /// Serve the routes of a plain `axum::Router` next to this app's: the way in for an existing
    /// axum application. They are **not documented** in OpenAPI (lesto never saw their handler
    /// types), but they get everything else: the problem rendering, the panic catcher, the
    /// request span, the timeout.
    ///
    /// Panics like `axum::Router::merge` when a path is registered on both sides.
    pub fn merge(mut self, router: Router<S>) -> Self {
        self.router = self.router.merge(router);
        self
    }

    /// [`merge`](Self::merge) under `prefix`, like `axum::Router::nest`: `/users` in `router`
    /// is served at `{prefix}/users`. Not documented in OpenAPI either.
    ///
    /// Panics like `axum::Router::nest` on an invalid prefix.
    pub fn nest_router(mut self, prefix: &str, router: Router<S>) -> Self {
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
            shutdown_timeout: self.shutdown_timeout,
            timeout: self.timeout,
            body_limit: self.body_limit,
            cors: self.cors,
            #[cfg(feature = "compression")]
            compression: self.compression,
            request_id: self.request_id,
            #[cfg(feature = "oidc")]
            oidc: self.oidc,
            #[cfg(feature = "oidc")]
            protect: self.protect,
            #[cfg(feature = "mcp")]
            mcp: self.mcp,
        }
    }

    // ---- output -----------------------------------------------------------------------

    /// The OpenAPI document for everything registered so far.
    pub fn openapi(&self) -> OpenApi {
        let mut spec = self.spec.clone();
        let mut generator = schema_generator();
        let mut security_schemes = self.security_schemes.clone();
        for (path, pending) in &self.operations {
            #[cfg(feature = "oidc")]
            let protected = self
                .protect
                .as_ref()
                .map(|protect| protected(pending.clone(), protect));
            #[cfg(feature = "oidc")]
            let pending = protected.as_ref().unwrap_or(pending);
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
        #[cfg(feature = "mcp")]
        let serves_mcp = self.mcp.is_some();
        #[cfg(not(feature = "mcp"))]
        let serves_mcp = false;
        let spec = (self.openapi_url.is_some() || serves_mcp).then(|| self.openapi());

        // The MCP route is added once the router is complete, since tool calls go through it;
        // being added last, it receives the same layers here, one by one.
        #[cfg(feature = "mcp")]
        let mut mcp = match (self.mcp, &spec) {
            (Some(config), Some(spec)) => {
                #[cfg(feature = "oidc")]
                let protected = self.protect.is_some();
                #[cfg(not(feature = "oidc"))]
                let protected = false;
                let endpoint = crate::mcp::Endpoint::new(config, &self.operations, spec, protected);
                let route = endpoint.method_router();
                Some((endpoint, route))
            }
            _ => None,
        };
        macro_rules! mcp_layer {
            ($layer:expr) => {
                #[cfg(feature = "mcp")]
                {
                    mcp = mcp.map(|(endpoint, route)| (endpoint, route.layer($layer)));
                }
            };
        }

        let mut router = self.router;
        // Before the documentation routes are added: those stay public.
        #[cfg(feature = "oidc")]
        if let Some(protect) = &self.protect {
            router = router.layer(protect.layer());
            mcp_layer!(protect.layer());
        }

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

        // Opt-in layers get a `Router::layer` call of their own, so an application that does not
        // ask for them pays nothing. Added first, they sit inside the stack below: a timeout
        // answer still goes through the problem layer.
        if let Some(limit) = self.timeout {
            router = router.layer(crate::layers::TimeoutLayer::new(limit));
            mcp_layer!(crate::layers::TimeoutLayer::new(limit));
        }
        if let Some(body_limit) = self.body_limit {
            router = router.layer(body_limit);
            mcp_layer!(body_limit);
        }
        #[cfg(feature = "oidc")]
        if let Some(oidc) = self.oidc {
            mcp_layer!(axum::Extension(oidc.clone()));
            router = router.layer(axum::Extension(oidc));
        }

        // One `Router::layer` call, not three: axum re-boxes every route (and its future) on
        // each one, so the layers are stacked first and added together.
        let layers = tower_layer::Stack::new(
            tower_layer::Stack::new(crate::layers::CatchPanicLayer, crate::layers::ProblemLayer),
            // Outermost, so the span covers the fallbacks, the panic catcher and the rendering
            // of the problem body — everything the client waits for. It still runs *inside*
            // routing, which is what makes `MatchedPath` (and so `http.route`) available.
            crate::layers::RequestSpanLayer::with(self.trace),
        );
        mcp_layer!(layers.clone());
        router = router.layer(layers);

        #[cfg(feature = "mcp")]
        if let Some((endpoint, route)) = mcp {
            endpoint.set_router(router.clone());
            router = router.route(&endpoint.path(), route);
        }

        // Outside the shared stack, opt-in: CORS headers and compression apply to every
        // response, problems included, and the request span sees the request id.
        if self.request_id {
            let header = http::HeaderName::from_static("x-request-id");
            router = router.layer(tower_layer::Stack::new(
                tower_http::request_id::PropagateRequestIdLayer::new(header.clone()),
                tower_http::request_id::SetRequestIdLayer::new(
                    header,
                    tower_http::request_id::MakeRequestUuid,
                ),
            ));
        }
        #[cfg(feature = "compression")]
        if self.compression {
            router = router.layer(tower_http::compression::CompressionLayer::new());
        }
        if let Some(cors) = self.cors {
            router = router.layer(cors);
        }
        router
    }
}

#[cfg(feature = "oidc")]
impl<S> App<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Enforce and document [`App::protect`] on the routes registered so far: done when the app
    /// is nested, since its routes are then out of its hands.
    fn apply_protection(mut self) -> Self {
        if let Some(protect) = self.protect.take() {
            self.router = self.router.layer(protect.layer());
            for (_, pending) in &mut self.operations {
                *pending = protected(pending.clone(), &protect);
            }
        }
        self
    }
}

/// `pending` as [`App::protect`] documents it: the `openIdConnect` requirement with the scopes,
/// the `403` when there are scopes, and no `public` exemption (the layer makes none).
#[cfg(feature = "oidc")]
fn protected(mut pending: PendingOperation, protect: &crate::oidc::Protect) -> PendingOperation {
    pending.meta.public = false;
    let requirement = (crate::oidc::SCHEME_NAME.to_string(), protect.scopes.clone());
    if !pending.meta.security.contains(&requirement) {
        pending.meta.security.push(requirement);
    }
    if !protect.scopes.is_empty() && !pending.meta.error_statuses.contains(&403) {
        pending.meta.error_statuses.push(403);
    }
    pending
}

impl App<()> {
    /// Serve on the address the environment says (see [`bind_address`]) or on a socket
    /// inherited from the parent process (`lesto dev`, systemd socket activation, `systemfd`;
    /// see [`listener`]), until `SIGTERM` or `Ctrl-C`.
    ///
    /// Shutdown is graceful: the listener closes at once, requests already in flight run to
    /// completion, then the future resolves — after [`shutdown_timeout`](Self::shutdown_timeout)
    /// at the latest (30 s by default). Requests still running then are dropped when `main`
    /// returns and the runtime shuts down.
    ///
    /// With `LESTO_OPENAPI_PATH` set, nothing is served: the OpenAPI document is written to that
    /// file and this returns `Ok(())`. That is how `lesto openapi` reads the document of an
    /// application without a flag in its `main`.
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
        if self.write_openapi_if_asked()? {
            return Ok(());
        }
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
        if self.write_openapi_if_asked()? {
            return Ok(());
        }
        // Held until the server is done, so the last spans are flushed after the in-flight
        // requests finish. Does nothing unless the `otel` feature is on, the environment names
        // a collector, and the application installed no subscriber of its own.
        #[cfg(feature = "otel")]
        let _telemetry = crate::otel::auto_init(&self.spec.info.title);
        let timeout = self.shutdown_timeout;
        let (started, shutdown_started) = tokio::sync::oneshot::channel::<()>();
        let shutdown = async move {
            shutdown.await;
            let _ = started.send(());
        };
        let server = axum::serve(listener, self.into_router())
            .with_graceful_shutdown(shutdown)
            .into_future();
        let Some(timeout) = timeout else {
            return server.await;
        };
        let deadline = async move {
            match shutdown_started.await {
                Ok(()) => tokio::time::sleep(timeout).await,
                // The server finished without a shutdown signal: nothing to time.
                Err(_) => std::future::pending().await,
            }
        };
        tokio::select! {
            result = server => result,
            () = deadline => {
                tracing::warn!(
                    timeout_ms = timeout.as_millis() as u64,
                    "shutdown deadline reached, abandoning requests still in flight"
                );
                Ok(())
            }
        }
    }

    /// `LESTO_OPENAPI_PATH` set: write the document there instead of serving.
    fn write_openapi_if_asked(&self) -> std::io::Result<bool> {
        self.write_openapi_to(std::env::var_os(OPENAPI_PATH_VAR))
    }

    fn write_openapi_to(&self, path: Option<std::ffi::OsString>) -> std::io::Result<bool> {
        let Some(path) = path else {
            return Ok(false);
        };
        std::fs::write(&path, self.openapi_json())?;
        Ok(true)
    }
}

/// Set by `lesto openapi`: [`App::serve`] writes the OpenAPI document to this file and returns
/// instead of serving.
const OPENAPI_PATH_VAR: &str = "LESTO_OPENAPI_PATH";

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
    bind_address_from(|name| std::env::var(name).ok())
}

/// [`bind_address`] against any source of variables, so tests never write the environment.
fn bind_address_from(get: impl Fn(&str) -> Option<String>) -> std::io::Result<(String, u16)> {
    let host = get("LESTO_HOST")
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = match get("LESTO_PORT").or_else(|| get("PORT")) {
        Some(raw) => raw.trim().parse::<u16>().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("LESTO_PORT/PORT must be a port number, got `{raw}`"),
            )
        })?,
        None => 8000,
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

/// An existing `axum::Router` as the starting point of an [`App`]: `App::from(router)` is
/// `App::new().merge(router)`.
impl<S> From<Router<S>> for App<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn from(router: Router<S>) -> Self {
        App::new().merge(router)
    }
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
    use super::{App, bind_address_from};

    #[test]
    fn openapi_is_written_only_when_asked() {
        let app = App::new().title("Written");
        assert!(!app.write_openapi_to(None).unwrap());
        let path = std::env::temp_dir().join(format!("lesto-openapi-test-{}", std::process::id()));
        assert!(app.write_openapi_to(Some(path.clone().into())).unwrap());
        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(written, app.openapi_json());
    }

    fn bind_address(vars: &[(&str, &str)]) -> std::io::Result<(String, u16)> {
        bind_address_from(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn bind_address_from_env() {
        assert_eq!(bind_address(&[]).unwrap(), ("127.0.0.1".to_string(), 8000));
        assert_eq!(
            bind_address(&[("PORT", "9000")]).unwrap().1,
            9000,
            "PORT is the fallback"
        );
        assert_eq!(
            bind_address(&[
                ("PORT", "9000"),
                ("LESTO_PORT", "8765"),
                ("LESTO_HOST", "0.0.0.0")
            ])
            .unwrap(),
            ("0.0.0.0".to_string(), 8765)
        );
        let err = bind_address(&[("LESTO_PORT", "nope")]).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}

//! The Model Context Protocol: operations marked `mcp = "tool"`, `"resource"` or `"prompt"` served
//! to agents (feature `mcp`).
//!
//! ```no_run
//! use lesto::prelude::*;
//! use lesto::mcp::Mcp;
//!
//! #[derive(Deserialize, JsonSchema, Validate)]
//! struct Search {
//!     /// Words the note must contain.
//!     #[garde(length(min = 1))]
//!     q: String,
//! }
//!
//! /// Search the notes.
//! #[lesto::get("/notes/search", mcp = "tool")]
//! async fn search_notes(Query(search): Query<Search>) -> Json<Vec<String>> {
//!     Json(vec![search.q])
//! }
//!
//! #[lesto::main]
//! async fn main() -> std::io::Result<()> {
//!     App::new()
//!         .routes(routes![search_notes])
//!         .mcp(Mcp::new())
//!         .serve()
//!         .await
//! }
//! ```
//!
//! A tool call becomes an HTTP request to the application's own router, built from the
//! operation's OpenAPI description: path and query parameters and the JSON body's properties
//! are the tool's arguments, side by side. Reading a resource is a `GET` of the path in its URI
//! (`lesto://notes/notes/7` → `GET /notes/7`), and getting a prompt a `GET` with the prompt's
//! arguments as path and query parameters; the route returns a [`Prompt`]. So a call runs the
//! same extractors, validation,
//! authentication, layers and tracing as the route does over HTTP: a validation failure comes
//! back to the agent as the `422` problem (`isError: true`), and a `401` from the route becomes
//! the `401` of the MCP request, which is what starts an MCP client's OAuth flow. The headers of
//! the MCP request (`Authorization`, API keys, cookies, `traceparent`) are forwarded; the inner
//! request carries an [`McpCall`] extension.
//!
//! A resource's route can set `Cache-Control` to let clients cache what they read: `max-age`
//! and `public`/`private` become 2026-07-28's `ttlMs` and `cacheScope`.
//!
//! The endpoint speaks MCP 2026-07-28, which is stateless, and also 2025-11-25 and 2025-06-18
//! for clients that still open with `initialize` ([`Mcp::legacy`]). It keeps no session in
//! either case and answers every request with a single JSON response, so it runs unchanged
//! behind a load balancer or on AWS Lambda.

mod catalog;
mod dispatch;
mod prompt;
mod protocol;

pub use prompt::{Prompt, PromptContent, PromptMessage, Role};

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use serde_json::{Map, Value, json};
use tower_service::Service;

use crate::error::HttpError;
use crate::openapi::OpenApi;
use crate::route::PendingOperation;
use catalog::Catalog;
use dispatch::Collected;
use protocol::{Era, Message, RpcError};

/// Default of [`Mcp::list_ttl`].
pub const DEFAULT_LIST_TTL: Duration = Duration::from_secs(300);

/// The MCP endpoint's configuration, for [`App::mcp`](crate::App::mcp).
///
/// Which operations are exposed is decided on the routes (`mcp = "tool"`, `"resource"`,
/// `"prompt"`), not here: turning the endpoint on never exposes a route by accident.
#[derive(Debug, Clone)]
pub struct Mcp {
    path: String,
    instructions: Option<String>,
    allowed_origins: Vec<String>,
    list_ttl: Duration,
    legacy: bool,
    resource_base: Option<String>,
}

impl Default for Mcp {
    fn default() -> Self {
        Self::new()
    }
}

impl Mcp {
    /// The endpoint at `/mcp`, legacy clients served, lists cacheable for five minutes.
    pub fn new() -> Self {
        Self {
            path: "/mcp".into(),
            instructions: None,
            allowed_origins: Vec::new(),
            list_ttl: DEFAULT_LIST_TTL,
            legacy: true,
            resource_base: None,
        }
    }

    /// Serve the endpoint at `path` instead of `/mcp`.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Guidance for the model on how to use this server, sent in `server/discover` (and in
    /// `initialize` for legacy clients).
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Allow browser pages from these origins (`https://app.example.com`) to call the endpoint.
    ///
    /// A request without `Origin` (every non-browser client) is always allowed, and so is a page
    /// on a loopback origin (`http://localhost:6274`) calling the endpoint on a loopback host.
    /// Any other origin, the endpoint's own public one included, is refused with `403`: that is
    /// what the specification requires against DNS rebinding, where a hostile page's origin and
    /// the `Host` it sends agree with each other.
    pub fn allowed_origins<I, O>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = O>,
        O: Into<String>,
    {
        self.allowed_origins = origins.into_iter().map(Into::into).collect();
        self
    }

    /// How long a client may cache the lists (tools, resources, templates, prompts) and
    /// `server/discover` (2026-07-28's `ttlMs`). They are fixed for the life of the process, so
    /// this only bounds how long a redeploy takes to show up.
    pub fn list_ttl(mut self, ttl: Duration) -> Self {
        self.list_ttl = ttl;
        self
    }

    /// Also serve clients of MCP 2025-11-25 and 2025-06-18, which open with `initialize`
    /// (default `true`). With `false` they get an error naming the supported versions.
    pub fn legacy(mut self, legacy: bool) -> Self {
        self.legacy = legacy;
        self
    }

    /// Prefix of resource URIs, followed by the route's path (default `lesto://{title}`, the
    /// app's title in lowercase with `-` for anything but letters and digits): with the default,
    /// `GET /notes/{id}` of the app "Notes" is `lesto://notes/notes/{id}`. Also names a binary
    /// tool result.
    pub fn resource_base(mut self, base: impl Into<String>) -> Self {
        self.resource_base = Some(base.into());
        self
    }
}

/// Marks a request made over MCP (a tool call, a resource read, a prompt): in the request
/// extensions of every inner request.
#[derive(Debug, Clone)]
pub struct McpCall {
    name: String,
}

impl McpCall {
    /// The name of the tool, resource or prompt.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// What the endpoint serves, computed once when the router is built.
struct Server {
    config: Mcp,
    catalog: Catalog,
    server_info: Value,
    resource_base: String,
    /// `cacheScope` of lists: `private` when the endpoint is behind `App::protect`.
    cache_scope: &'static str,
}

/// The endpoint's handler state: the server description and the router tool calls go through.
struct Shared<S> {
    server: Server,
    /// Every route and layer of the app, without the endpoint itself: set by
    /// [`Endpoint::set_router`] once the router is complete.
    router: OnceLock<Router<S>>,
    /// `router` with the state, built by the first tool call (the state only arrives then).
    ready: OnceLock<Router>,
}

/// The `/mcp` endpoint while the app's router is being built.
pub(crate) struct Endpoint<S> {
    shared: Arc<Shared<S>>,
}

impl<S> Endpoint<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// The tools of `operations`, described from `spec`. `protected`: the endpoint is behind
    /// `App::protect`, so what it lists is private to the caller.
    pub(crate) fn new(
        config: Mcp,
        operations: &[(String, PendingOperation)],
        spec: &OpenApi,
        protected: bool,
    ) -> Self {
        let resource_base = config
            .resource_base
            .clone()
            .unwrap_or_else(|| format!("lesto://{}", slug(&spec.info.title)));
        let catalog = Catalog::build(operations, spec, &resource_base);
        let server = Server {
            server_info: json!({ "name": spec.info.title, "version": spec.info.version }),
            config,
            catalog,
            resource_base,
            cache_scope: if protected { "private" } else { "public" },
        };
        Endpoint {
            shared: Arc::new(Shared {
                server,
                router: OnceLock::new(),
                ready: OnceLock::new(),
            }),
        }
    }

    pub(crate) fn path(&self) -> String {
        self.shared.server.config.path.clone()
    }

    /// The route: `POST` for messages, `405` for any other method (no SSE stream, no session to
    /// delete).
    pub(crate) fn method_router(&self) -> MethodRouter<S> {
        let shared = self.shared.clone();
        axum::routing::post(
            move |State(state): State<S>, uri: Uri, headers: HeaderMap, body: Bytes| {
                let shared = shared.clone();
                async move { shared.handle(state, uri, headers, body).await }
            },
        )
        .fallback(|| async {
            let mut response = HttpError::new(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
                .into_response();
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("POST"));
            response
        })
    }

    /// The finished router, without this endpoint: where tool calls go.
    pub(crate) fn set_router(&self, router: Router<S>) {
        let _ = self.shared.router.set(router);
    }
}

/// Whether `authority` (`host[:port]`) names this machine: `localhost`, `127.0.0.0/8` or `[::1]`.
fn is_loopback(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((host, _)) => host,
            None => return false,
        }
    } else {
        authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host)
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// `My Notes API` → `my-notes-api`.
fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "app".into()
    } else {
        slug.into()
    }
}

impl<S> Shared<S>
where
    S: Clone + Send + Sync + 'static,
{
    async fn handle(&self, state: S, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
        if !self.origin_allowed(&uri, &headers) {
            return RpcError::new(
                StatusCode::FORBIDDEN,
                protocol::INVALID_REQUEST,
                "origin not allowed",
            )
            .into_response(None, None);
        }
        let request = match protocol::parse(&body) {
            Ok(Message::Request(request)) => request,
            Ok(Message::Notification) => return StatusCode::ACCEPTED.into_response(),
            Err(error) => return error.into_response(None, None),
        };
        let id = request.id.as_ref();
        let era = match protocol::select_era(&request, &headers, self.server.config.legacy) {
            Ok(era) => era,
            Err(error) => {
                let era = request.params.get("_meta").is_some().then_some(Era::Modern);
                return error.into_response(era, id);
            }
        };
        let params = &request.params;
        let method = request.method.as_str();
        // A kind the app does not expose is not in the capabilities, and its methods do not exist.
        let catalog = &self.server.catalog;
        let offered = match method.split_once('/').map(|(kind, _)| kind) {
            Some("tools") => !catalog.tools.is_empty(),
            Some("resources") => !catalog.resources.is_empty(),
            Some("prompts") => !catalog.prompts.is_empty(),
            _ => true,
        };
        if !offered {
            return RpcError::method_not_found(era, method).into_response(Some(era), id);
        }
        let result = match (era, method) {
            (Era::Modern, "server/discover") => Ok(self.discover()),
            (Era::Legacy, "initialize") => Ok(self.initialize(params)),
            (Era::Legacy, "ping") => Ok(Map::new()),
            (_, "tools/list") => Ok(self.list(era, "tools", self.tools(era))),
            (_, "resources/list") => Ok(self.list(era, "resources", self.resources(false))),
            (_, "resources/templates/list") => {
                Ok(self.list(era, "resourceTemplates", self.resources(true)))
            }
            (_, "prompts/list") => Ok(self.list(era, "prompts", self.prompts())),
            (_, "tools/call") => {
                self.call_tool(era, params, &headers, self.router(state))
                    .await
            }
            (_, "resources/read") => {
                self.read_resource(era, params, &headers, self.router(state))
                    .await
            }
            (_, "prompts/get") => self.get_prompt(params, &headers, self.router(state)).await,
            (_, method) => Err(RpcError::method_not_found(era, method).into()),
        };
        match result {
            Ok(result) => protocol::result_response(era, id, result, &self.server.server_info),
            Err(Outcome::Response(response)) => *response,
            Err(Outcome::Error(error)) => error.into_response(Some(era), id),
        }
    }

    /// The app's router with the state: built by the first call that needs it, since the state
    /// only arrives with a request.
    fn router(&self, state: S) -> &Router {
        self.ready.get_or_init(|| {
            self.router
                .get()
                .cloned()
                .unwrap_or_default()
                .with_state(state)
        })
    }

    /// No `Origin` (not a browser), an allowed origin, or a page on this machine calling a
    /// server on this machine.
    ///
    /// Comparing `Origin` with `Host` would not do: under DNS rebinding a page on `evil.com`,
    /// whose name now resolves to 127.0.0.1, sends `Origin: http://evil.com:8000` *and*
    /// `Host: evil.com:8000`. What the attacker cannot choose is a loopback origin, since the
    /// page's origin is the name it was loaded from.
    fn origin_allowed(&self, uri: &Uri, headers: &HeaderMap) -> bool {
        let Some(origin) = headers.get(header::ORIGIN) else {
            return true;
        };
        let Ok(origin) = origin.to_str() else {
            return false;
        };
        if self
            .server
            .config
            .allowed_origins
            .iter()
            .any(|o| o.eq_ignore_ascii_case(origin))
        {
            return true;
        }
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .or_else(|| uri.authority().map(|a| a.as_str()));
        let origin_authority = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"));
        matches!(
            (origin_authority, host),
            (Some(origin), Some(host)) if is_loopback(origin) && is_loopback(host)
        )
    }

    fn capabilities(&self) -> Value {
        let mut capabilities = Map::new();
        let catalog = &self.server.catalog;
        if !catalog.tools.is_empty() {
            capabilities.insert("tools".into(), json!({ "listChanged": false }));
        }
        if !catalog.resources.is_empty() {
            capabilities.insert("resources".into(), json!({ "listChanged": false }));
        }
        if !catalog.prompts.is_empty() {
            capabilities.insert("prompts".into(), json!({ "listChanged": false }));
        }
        Value::Object(capabilities)
    }

    /// 2026-07-28's caching fields: the catalog is fixed for the life of the process.
    fn cacheable(&self, result: &mut Map<String, Value>) {
        let ttl = u64::try_from(self.server.config.list_ttl.as_millis()).unwrap_or(u64::MAX);
        result.insert("ttlMs".into(), ttl.into());
        result.insert("cacheScope".into(), self.server.cache_scope.into());
    }

    fn discover(&self) -> Map<String, Value> {
        let mut result = Map::new();
        result.insert(
            "supportedVersions".into(),
            protocol::supported_versions(self.server.config.legacy).into(),
        );
        result.insert("capabilities".into(), self.capabilities());
        if let Some(instructions) = &self.server.config.instructions {
            result.insert("instructions".into(), instructions.clone().into());
        }
        self.cacheable(&mut result);
        result
    }

    fn initialize(&self, params: &Map<String, Value>) -> Map<String, Value> {
        let mut result = Map::new();
        result.insert(
            "protocolVersion".into(),
            protocol::negotiate_legacy(params).into(),
        );
        result.insert("capabilities".into(), self.capabilities());
        result.insert("serverInfo".into(), self.server.server_info.clone());
        if let Some(instructions) = &self.server.config.instructions {
            result.insert("instructions".into(), instructions.clone().into());
        }
        result
    }

    /// A list result: `items` under `key`, cacheable in 2026-07-28.
    fn list(&self, era: Era, key: &str, items: Vec<Value>) -> Map<String, Value> {
        let mut result = Map::new();
        result.insert(key.into(), items.into());
        if era == Era::Modern {
            self.cacheable(&mut result);
        }
        result
    }

    fn tools(&self, era: Era) -> Vec<Value> {
        let modern = era == Era::Modern;
        let tools = &self.server.catalog.tools;
        tools.iter().map(|tool| tool.definition(modern)).collect()
    }

    /// The fixed resources, or the templates.
    fn resources(&self, templates: bool) -> Vec<Value> {
        let resources = &self.server.catalog.resources;
        resources
            .iter()
            .filter(|r| r.template == templates)
            .map(|r| r.definition.clone())
            .collect()
    }

    fn prompts(&self) -> Vec<Value> {
        let prompts = &self.server.catalog.prompts;
        prompts.iter().map(|p| p.definition.clone()).collect()
    }

    async fn call_tool(
        &self,
        era: Era,
        params: &Map<String, Value>,
        headers: &HeaderMap,
        router: &Router,
    ) -> Result<Map<String, Value>, Outcome> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("`name` must be the name of a tool"))?;
        let tool = self
            .server
            .catalog
            .tool(name)
            .ok_or_else(|| RpcError::invalid_params(format!("unknown tool: {name}")))?;
        let arguments = arguments(params)?;
        let meta = params.get("_meta").and_then(Value::as_object);
        let request = dispatch::request(&tool.target, arguments, headers, meta)
            .map_err(RpcError::invalid_params)?;
        let uri = format!("{}{}", self.server.resource_base, request.uri());
        let response = send(router, request, name).await?;
        Ok(dispatch::tool_result(
            tool,
            era == Era::Modern,
            &response,
            uri,
        ))
    }

    async fn read_resource(
        &self,
        era: Era,
        params: &Map<String, Value>,
        headers: &HeaderMap,
        router: &Router,
    ) -> Result<Map<String, Value>, Outcome> {
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("`uri` must be the URI of a resource"))?;
        let not_found = || Outcome::from(RpcError::resource_not_found(era, uri));
        // The path and query after the base, as the route receives them.
        let target = uri
            .strip_prefix(self.server.resource_base.as_str())
            .filter(|rest| rest.starts_with('/'))
            .ok_or_else(not_found)?;
        let target = target.split_once('#').map_or(target, |(before, _)| before);
        let path = target.split_once('?').map_or(target, |(path, _)| path);
        let resource = self.server.catalog.resource(path).ok_or_else(not_found)?;
        let meta = params.get("_meta").and_then(Value::as_object);
        let request = dispatch::build(&resource.target, target, None, headers, meta)
            .map_err(RpcError::invalid_params)?;
        let response = send(router, request, &resource.target.name).await?;
        if response.status == StatusCode::NOT_FOUND {
            return Err(not_found());
        }
        if !response.status.is_success() {
            return Err(route_failed(protocol::INTERNAL_ERROR, &response).into());
        }
        let mut result = dispatch::resource_result(uri, &response);
        if era == Era::Modern {
            let (ttl_ms, scope) = dispatch::resource_cache(&response.headers);
            result.insert("ttlMs".into(), ttl_ms.into());
            result.insert("cacheScope".into(), scope.into());
        }
        Ok(result)
    }

    async fn get_prompt(
        &self,
        params: &Map<String, Value>,
        headers: &HeaderMap,
        router: &Router,
    ) -> Result<Map<String, Value>, Outcome> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("`name` must be the name of a prompt"))?;
        let prompt = self
            .server
            .catalog
            .prompt(name)
            .ok_or_else(|| RpcError::invalid_params(format!("unknown prompt: {name}")))?;
        let arguments = arguments(params)?;
        let meta = params.get("_meta").and_then(Value::as_object);
        let request = dispatch::request(&prompt.target, arguments, headers, meta)
            .map_err(RpcError::invalid_params)?;
        let response = send(router, request, name).await?;
        // A 4xx is the arguments' fault (a missing or invalid one): the client can fix them.
        if response.status.is_client_error() {
            return Err(route_failed(protocol::INVALID_PARAMS, &response).into());
        }
        if !response.status.is_success() {
            return Err(route_failed(protocol::INTERNAL_ERROR, &response).into());
        }
        match serde_json::from_slice::<Value>(&response.bytes) {
            Ok(Value::Object(result)) => Ok(result),
            _ => {
                tracing::error!(
                    prompt = name,
                    "the prompt's route did not answer a JSON object"
                );
                Err(internal_error().into())
            }
        }
    }
}

/// `arguments` of a call: absent or an object.
fn arguments(params: &Map<String, Value>) -> Result<&Map<String, Value>, RpcError> {
    static EMPTY: OnceLock<Map<String, Value>> = OnceLock::new();
    match params.get("arguments") {
        None | Some(Value::Null) => Ok(EMPTY.get_or_init(Map::new)),
        Some(Value::Object(arguments)) => Ok(arguments),
        Some(_) => Err(RpcError::invalid_params("`arguments` must be an object")),
    }
}

/// Send the inner request through the app's router and read the response. An authentication
/// challenge ends the MCP request with the route's own response.
async fn send(
    router: &Router,
    request: axum::extract::Request,
    name: &str,
) -> Result<Collected, Outcome> {
    let mut router = router.clone();
    std::future::poll_fn(|cx| Service::<axum::extract::Request>::poll_ready(&mut router, cx))
        .await
        .unwrap_or_else(|never| match never {});
    let response = match router.call(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    if dispatch::is_auth_challenge(&response) {
        return Err(Outcome::Response(Box::new(response)));
    }
    Collected::read(response).await.map_err(|message| {
        tracing::error!(name, "{message}");
        internal_error().into()
    })
}

fn internal_error() -> RpcError {
    RpcError::new(StatusCode::OK, protocol::INTERNAL_ERROR, "the call failed")
}

/// The route behind a resource or a prompt failed: `code`, with the problem's detail as the
/// message and the problem as `data`.
fn route_failed(code: i64, response: &Collected) -> RpcError {
    let message = response
        .problem_message()
        .unwrap_or_else(|| format!("the route answered {}", response.status));
    RpcError::failed(code, message, response.error_data())
}

/// How a request ends when it does not produce a result.
enum Outcome {
    /// A JSON-RPC error.
    Error(RpcError),
    /// The inner response itself (an authentication challenge).
    Response(Box<Response>),
}

impl From<RpcError> for Outcome {
    fn from(error: RpcError) -> Self {
        Outcome::Error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_authorities() {
        for yes in [
            "localhost",
            "localhost:8000",
            "127.0.0.1:8000",
            "127.1.2.3",
            "[::1]:8000",
            "LOCALHOST",
        ] {
            assert!(is_loopback(yes), "{yes}");
        }
        for no in [
            "evil.com:8000",
            "localhost.evil.com",
            "10.0.0.1",
            "[::2]:80",
            "[::1",
            "",
        ] {
            assert!(!is_loopback(no), "{no}");
        }
    }

    #[test]
    fn slug_keeps_letters_and_digits() {
        assert_eq!(slug("My Notes API v2"), "my-notes-api-v2");
        assert_eq!(slug("  "), "app");
    }
}

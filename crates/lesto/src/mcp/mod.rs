//! The Model Context Protocol: operations marked `mcp = "tool"` served to agents (feature `mcp`).
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
//! are the tool's arguments, side by side. So a call runs the same extractors, validation,
//! authentication, layers and tracing as the route does over HTTP: a validation failure comes
//! back to the agent as the `422` problem (`isError: true`), and a `401` from the route becomes
//! the `401` of the MCP request, which is what starts an MCP client's OAuth flow. The headers of
//! the MCP request (`Authorization`, API keys, cookies, `traceparent`) are forwarded; the inner
//! request carries an [`McpCall`] extension.
//!
//! The endpoint speaks MCP 2026-07-28, which is stateless, and also 2025-11-25 and 2025-06-18
//! for clients that still open with `initialize` ([`Mcp::legacy`]). It keeps no session in
//! either case and answers every request with a single JSON response, so it runs unchanged
//! behind a load balancer or on AWS Lambda.

mod catalog;
mod dispatch;
mod protocol;

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
use protocol::{Era, Message, RpcError};

/// Default of [`Mcp::list_ttl`].
pub const DEFAULT_LIST_TTL: Duration = Duration::from_secs(300);

/// The MCP endpoint's configuration, for [`App::mcp`](crate::App::mcp).
///
/// Which operations are exposed is decided on the routes (`mcp = "tool"`), not here: turning
/// the endpoint on never exposes a route by accident.
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

    /// Guidance for the model on how to use these tools, sent in `server/discover` (and in
    /// `initialize` for legacy clients).
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Allow browser pages from these origins (`https://app.example.com`) to call the endpoint.
    ///
    /// A request without `Origin` (every non-browser client) and one from the endpoint's own
    /// host are always allowed; any other origin is refused with `403`, as the specification
    /// requires against DNS rebinding.
    pub fn allowed_origins<I, O>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = O>,
        O: Into<String>,
    {
        self.allowed_origins = origins.into_iter().map(Into::into).collect();
        self
    }

    /// How long a client may cache `tools/list` and `server/discover` (2026-07-28's `ttlMs`).
    /// The tools are fixed for the life of the process, so this only bounds how long a
    /// redeploy takes to show up.
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

    /// Prefix of the URI naming a binary tool result (default `lesto://{title}`, the app's
    /// title in lowercase with `-` for anything but letters and digits).
    pub fn resource_base(mut self, base: impl Into<String>) -> Self {
        self.resource_base = Some(base.into());
        self
    }
}

/// Marks a request made by an MCP tool call: in the request extensions of every inner request.
#[derive(Debug, Clone)]
pub struct McpCall {
    name: String,
}

impl McpCall {
    /// The name of the tool that was called.
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
        let catalog = Catalog::build(operations, spec);
        let resource_base = config
            .resource_base
            .clone()
            .unwrap_or_else(|| format!("lesto://{}", slug(&spec.info.title)));
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
        let result = match (era, request.method.as_str()) {
            (Era::Modern, "server/discover") => Ok(self.discover()),
            (Era::Legacy, "initialize") => Ok(self.initialize(&request.params)),
            (Era::Legacy, "ping") => Ok(Map::new()),
            (_, "tools/list") => Ok(self.list_tools(era)),
            (_, "tools/call") => {
                let router = self.ready.get_or_init(|| {
                    self.router
                        .get()
                        .cloned()
                        .unwrap_or_default()
                        .with_state(state)
                });
                match self.call_tool(era, &request.params, &headers, router).await {
                    Ok(result) => Ok(result),
                    Err(Outcome::Response(response)) => return *response,
                    Err(Outcome::Error(error)) => Err(error),
                }
            }
            (_, method) => Err(RpcError::method_not_found(era, method)),
        };
        match result {
            Ok(result) => protocol::result_response(era, id, result, &self.server.server_info),
            Err(error) => error.into_response(Some(era), id),
        }
    }

    /// No `Origin` (not a browser), the endpoint's own host, or an allowed origin.
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
            .any(|o| o == origin)
        {
            return true;
        }
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .or_else(|| uri.authority().map(|a| a.as_str()));
        let authority = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"));
        matches!((authority, host), (Some(a), Some(h)) if a.eq_ignore_ascii_case(h))
    }

    fn capabilities(&self) -> Value {
        let mut capabilities = Map::new();
        if !self.server.catalog.tools.is_empty() {
            capabilities.insert("tools".into(), json!({ "listChanged": false }));
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

    fn list_tools(&self, era: Era) -> Map<String, Value> {
        let modern = era == Era::Modern;
        let tools: Vec<Value> = self
            .server
            .catalog
            .tools
            .iter()
            .map(|tool| tool.definition(modern))
            .collect();
        let mut result = Map::new();
        result.insert("tools".into(), tools.into());
        if modern {
            self.cacheable(&mut result);
        }
        result
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
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => return Err(RpcError::invalid_params("`arguments` must be an object").into()),
        };
        let meta = params.get("_meta").and_then(Value::as_object);
        let request =
            dispatch::request(tool, arguments, headers, meta).map_err(RpcError::invalid_params)?;
        let uri = format!("{}{}", self.server.resource_base, request.uri());

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
        dispatch::result(tool, era == Era::Modern, response, uri)
            .await
            .map_err(|message| {
                tracing::error!(tool = name, "{message}");
                RpcError::new(
                    StatusCode::OK,
                    protocol::INTERNAL_ERROR,
                    "the tool call failed",
                )
                .into()
            })
    }
}

/// How a `tools/call` ends when it does not produce a result.
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
    fn slug_keeps_letters_and_digits() {
        assert_eq!(slug("My Notes API v2"), "my-notes-api-v2");
        assert_eq!(slug("  "), "app");
    }
}

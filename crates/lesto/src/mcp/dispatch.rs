//! A `tools/call`, `resources/read` or `prompts/get` as an HTTP request to the application's own
//! router, and the response as the result.

use axum::body::Body;
use axum::body::Bytes;
use axum::extract::Request;
use axum::response::Response;
use base64::Engine;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use serde_json::{Map, Value, json};

use super::McpCall;
use super::catalog::{BodyArgs, Target, Tool};

/// Headers of the transport request that do not describe the inner one: framing, hop-by-hop,
/// and MCP's own.
fn skipped(name: &HeaderName) -> bool {
    matches!(
        *name,
        header::CONTENT_LENGTH
            | header::CONTENT_TYPE
            | header::CONTENT_ENCODING
            | header::ACCEPT
            | header::ACCEPT_ENCODING
            | header::CONNECTION
            | header::TE
            | header::TRAILER
            | header::TRANSFER_ENCODING
            | header::UPGRADE
            | header::ORIGIN
    ) || name.as_str().starts_with("mcp-")
        || name.as_str() == "keep-alive"
        || name.as_str().starts_with("proxy-")
}

/// Trace context a modern client may carry in `_meta` instead of (or besides) the HTTP headers.
/// It belongs to this call, so it wins on the inner request.
const META_TRACE_KEYS: [&str; 3] = ["traceparent", "tracestate", "baggage"];

/// The inner request for `target` called with `arguments`. `Err` is the message of an
/// invalid-params error.
pub(crate) fn request(
    target: &Target,
    arguments: &Map<String, Value>,
    transport: &HeaderMap,
    meta: Option<&Map<String, Value>>,
) -> Result<Request, String> {
    let path = fill_path(target, arguments)?;
    let query = query_string(target, arguments)?;
    let uri = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };
    let body = match &target.body {
        BodyArgs::None => None,
        BodyArgs::Flat => {
            let body: Map<String, Value> = arguments
                .iter()
                .filter(|(key, _)| {
                    !target.path_params.iter().any(|p| &p.name == *key)
                        && !target.query_params.contains(key)
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Some(Value::Object(body))
        }
        BodyArgs::Wrapped { .. } => arguments.get("body").filter(|v| !v.is_null()).cloned(),
    };
    build(target, &uri, body, transport, meta)
}

/// The inner request to `uri` (path and query) for `target`, with the transport's headers.
pub(crate) fn build(
    target: &Target,
    uri: &str,
    body: Option<Value>,
    transport: &HeaderMap,
    meta: Option<&Map<String, Value>>,
) -> Result<Request, String> {
    let mut request = Request::builder()
        .method(target.method.clone())
        .uri(uri)
        .body(match &body {
            Some(body) => Body::from(serde_json::to_vec(body).expect("a JSON value serializes")),
            None => Body::empty(),
        })
        .map_err(|e| format!("the arguments do not make a valid request to {uri}: {e}"))?;
    let headers = request.headers_mut();
    for (name, value) in transport {
        if !skipped(name) {
            headers.append(name.clone(), value.clone());
        }
    }
    if let Some(meta) = meta {
        for key in META_TRACE_KEYS {
            if let Some(value) = meta
                .get(key)
                .and_then(Value::as_str)
                .and_then(|v| HeaderValue::from_str(v).ok())
            {
                headers.insert(HeaderName::from_static(key), value);
            }
        }
    }
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    if body.is_some() {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    request.extensions_mut().insert(McpCall {
        name: target.name.clone(),
    });
    Ok(request)
}

/// A scalar argument as it appears in a path or a query string.
fn scalar(name: &str, value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(format!(
            "argument `{name}` must be a string, a number or a boolean"
        )),
    }
}

fn fill_path(target: &Target, arguments: &Map<String, Value>) -> Result<String, String> {
    let mut path = String::with_capacity(target.path.len());
    let mut rest = target.path.as_str();
    let mut params = target.path_params.iter();
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        path.push_str(&rest[..start]);
        rest = &rest[start + end + 1..];
        let Some(param) = params.next() else {
            break;
        };
        let value = match arguments.get(&param.name) {
            None | Some(Value::Null) => {
                return Err(format!("missing required argument `{}`", param.name));
            }
            Some(value) => scalar(&param.name, value)?,
        };
        percent_encode(&value, param.wildcard, &mut path);
    }
    path.push_str(rest);
    Ok(path)
}

/// Percent-encode everything but RFC 3986's unreserved characters (and `/` for a wildcard,
/// which spans segments).
fn percent_encode(value: &str, keep_slash: bool, out: &mut String) {
    use std::fmt::Write as _;
    for byte in value.bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if unreserved || (keep_slash && byte == b'/') {
            out.push(byte as char);
        } else {
            // Writing into a `String` cannot fail.
            let _ = write!(out, "%{byte:02X}");
        }
    }
}

/// Query parameters the way `lesto::Query` reads them: arrays as repeated keys, `null` omitted.
fn query_string(target: &Target, arguments: &Map<String, Value>) -> Result<String, String> {
    let mut query = form_urlencoded::Serializer::new(String::new());
    for name in &target.query_params {
        match arguments.get(name) {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => {
                for item in items {
                    query.append_pair(name, &scalar(name, item)?);
                }
            }
            Some(value) => {
                query.append_pair(name, &scalar(name, value)?);
            }
        }
    }
    Ok(query.finish())
}

/// Whether the transport must answer with the inner response itself: a `401`, or a `403` asking
/// for more scope. Those are what make an MCP client start (or step up) its OAuth flow, and a
/// client only looks for them on the HTTP response.
pub(crate) fn is_auth_challenge(response: &Response) -> bool {
    match response.status() {
        StatusCode::UNAUTHORIZED => true,
        StatusCode::FORBIDDEN => response
            .headers()
            .get_all(header::WWW_AUTHENTICATE)
            .iter()
            .any(|v| v.to_str().is_ok_and(|v| v.contains("insufficient_scope"))),
        _ => false,
    }
}

/// An inner response, read: what the result builders need.
pub(crate) struct Collected {
    pub status: StatusCode,
    pub headers: HeaderMap,
    /// The media type without parameters, lowercase (`application/json`), empty if absent.
    pub essence: String,
    pub bytes: Bytes,
}

impl Collected {
    pub async fn read(response: Response) -> Result<Collected, String> {
        let (parts, body) = response.into_parts();
        let essence = parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .map_err(|e| format!("reading the response failed: {e}"))?;
        Ok(Collected {
            status: parts.status,
            headers: parts.headers,
            essence,
            bytes,
        })
    }

    fn is_json(&self) -> bool {
        self.essence == "application/json" || self.essence.ends_with("+json")
    }

    /// JSON, `text/*`, or no declared type: shown as text.
    fn is_text(&self) -> bool {
        self.is_json() || self.essence.starts_with("text/") || self.essence.is_empty()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    /// The body of a failed response as JSON-RPC error `data`: the problem, or the text.
    pub fn error_data(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or_else(|_| self.text().into())
    }

    /// The problem's `detail` (else its `title`), to put in a JSON-RPC error message.
    pub fn problem_message(&self) -> Option<String> {
        let problem: Value = serde_json::from_slice(&self.bytes).ok()?;
        problem
            .get("detail")
            .or_else(|| problem.get("title"))
            .and_then(Value::as_str)
            .map(String::from)
    }
}

/// The `tools/call` result for the inner response: its body as content, `isError` on failure,
/// `structuredContent` when the tool declares an output schema. `uri` names a binary body.
pub(crate) fn tool_result(
    tool: &Tool,
    modern: bool,
    response: &Collected,
    uri: String,
) -> Map<String, Value> {
    let mut result = Map::new();
    let mut content = Vec::new();
    let essence = response.essence.as_str();
    if response.bytes.is_empty() {
        // `204 No Content` and friends: nothing to show.
    } else if response.is_text() {
        if response.status.is_success()
            && response.is_json()
            && tool.structured(modern)
            && let Ok(value) = serde_json::from_slice::<Value>(&response.bytes)
            && (modern || value.is_object())
        {
            result.insert("structuredContent".into(), value);
        }
        content.push(json!({ "type": "text", "text": response.text() }));
    } else {
        let data = base64::engine::general_purpose::STANDARD.encode(&response.bytes);
        let item = if essence.starts_with("image/") || essence.starts_with("audio/") {
            let kind = if essence.starts_with("image/") {
                "image"
            } else {
                "audio"
            };
            json!({ "type": kind, "data": data, "mimeType": essence })
        } else {
            json!({
                "type": "resource",
                "resource": { "uri": uri, "mimeType": essence, "blob": data },
            })
        };
        content.push(item);
    }
    result.insert("content".into(), Value::Array(content));
    if !response.status.is_success() {
        result.insert("isError".into(), true.into());
    }
    result
}

/// The `resources/read` result for a successful inner response: one content item, text for
/// JSON and `text/*`, a base64 `blob` otherwise.
pub(crate) fn resource_result(uri: &str, response: &Collected) -> Map<String, Value> {
    let mut item = Map::new();
    item.insert("uri".into(), uri.into());
    if !response.essence.is_empty() {
        item.insert("mimeType".into(), response.essence.clone().into());
    }
    if response.is_text() {
        item.insert("text".into(), response.text().into());
    } else {
        let data = base64::engine::general_purpose::STANDARD.encode(&response.bytes);
        item.insert("blob".into(), data.into());
    }
    let mut result = Map::new();
    result.insert("contents".into(), json!([item]));
    result
}

/// 2026-07-28's caching fields for a resource, from the inner response's `Cache-Control`:
/// `max-age` is the time to live, `public` / `private` the scope. Without the header, nothing
/// may be cached (`0`, `private`): the data may change and may depend on the caller.
pub(crate) fn resource_cache(headers: &HeaderMap) -> (u64, &'static str) {
    let mut ttl_ms = 0;
    let mut scope = "private";
    let directives = headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim);
    let mut no_store = false;
    for directive in directives {
        let (name, value) = directive.split_once('=').unwrap_or((directive, ""));
        match name.to_ascii_lowercase().as_str() {
            "max-age" => {
                if let Ok(seconds) = value.trim_matches('"').parse::<u64>() {
                    ttl_ms = seconds.saturating_mul(1000);
                }
            }
            "public" => scope = "public",
            "no-store" | "no-cache" => no_store = true,
            _ => {}
        }
    }
    if no_store {
        ttl_ms = 0;
    }
    (ttl_ms, scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::catalog::PathParam;
    use serde_json::json;

    fn tool(path: &str, params: &[(&str, bool)], query: &[&str], body: BodyArgs) -> Target {
        Target {
            name: "t".into(),
            method: http::Method::POST,
            path: path.into(),
            path_params: params
                .iter()
                .map(|(name, wildcard)| PathParam {
                    name: name.to_string(),
                    wildcard: *wildcard,
                })
                .collect(),
            query_params: query.iter().map(|q| q.to_string()).collect(),
            body,
        }
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn arguments_are_split_into_path_query_and_body() {
        let tool = tool(
            "/notes/{id}/files/{*rest}",
            &[("id", false), ("rest", true)],
            &["tag"],
            BodyArgs::Flat,
        );
        let request = request(
            &tool,
            &args(json!({"id": "a b", "rest": "x/y z", "tag": ["p", "q"], "title": "T"})),
            &HeaderMap::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            request.uri().to_string(),
            "/notes/a%20b/files/x/y%20z?tag=p&tag=q"
        );
        assert_eq!(request.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(request.extensions().get::<McpCall>().unwrap().name(), "t");
    }

    #[test]
    fn missing_path_arguments_are_invalid_params() {
        let tool = tool("/notes/{id}", &[("id", false)], &[], BodyArgs::None);
        let err = request(&tool, &Map::new(), &HeaderMap::new(), None).unwrap_err();
        assert_eq!(err, "missing required argument `id`");
    }

    #[test]
    fn transport_headers_are_forwarded_except_framing_and_mcp() {
        let tool = tool("/notes", &[], &[], BodyArgs::None);
        let mut transport = HeaderMap::new();
        transport.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer t"));
        transport.insert("mcp-method", HeaderValue::from_static("tools/call"));
        transport.insert(header::CONTENT_LENGTH, HeaderValue::from_static("99"));
        transport.insert("traceparent", HeaderValue::from_static("from-header"));
        let meta = args(json!({"traceparent": "from-meta"}));
        let request = request(&tool, &Map::new(), &transport, Some(&meta)).unwrap();
        let headers = request.headers();
        assert_eq!(headers[header::AUTHORIZATION], "Bearer t");
        assert!(headers.get("mcp-method").is_none());
        assert!(headers.get(header::CONTENT_LENGTH).is_none());
        assert_eq!(headers["traceparent"], "from-meta");
        assert!(headers.get(header::CONTENT_TYPE).is_none());
    }

    #[test]
    fn resource_caching_follows_cache_control() {
        let mut headers = HeaderMap::new();
        assert_eq!(resource_cache(&headers), (0, "private"));
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
        assert_eq!(resource_cache(&headers), (60_000, "public"));
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=60, no-store"),
        );
        assert_eq!(resource_cache(&headers), (0, "private"));
    }
}

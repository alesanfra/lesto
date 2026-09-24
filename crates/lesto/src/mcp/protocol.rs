//! The JSON-RPC envelope and the parts of MCP that differ between protocol eras.
//!
//! *Modern* is 2026-07-28: no handshake, the version and the client's capabilities travel in
//! every request's `_meta`, and the transport mirrors the method and the tool name in headers.
//! *Legacy* is 2025-11-25 and 2025-06-18: an `initialize` handshake first, then the negotiated
//! version in the `MCP-Protocol-Version` header. lesto serves both on one endpoint, keeping no
//! state in either (see `docs/design/mcp.md`).

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use http::{HeaderMap, HeaderValue, StatusCode, header};
use serde_json::{Map, Value, json};

/// The modern revision.
pub(crate) const MODERN: &str = "2026-07-28";
/// Legacy revisions served without a session, newest first: the first is what `initialize`
/// negotiates down to.
pub(crate) const LEGACY: [&str; 2] = ["2025-11-25", "2025-06-18"];

/// `_meta` key of the protocol version on modern requests.
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// `_meta` key of the server's identity on modern results.
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

pub(crate) const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const METHOD_HEADER: &str = "mcp-method";
const NAME_HEADER: &str = "mcp-name";

// JSON-RPC and MCP error codes.
pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const INVALID_REQUEST: i64 = -32600;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;
pub(crate) const INTERNAL_ERROR: i64 = -32603;
/// Legacy "resource not found" (2025 revisions); modern uses `INVALID_PARAMS`.
const RESOURCE_NOT_FOUND: i64 = -32002;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Which revision a request is served under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Era {
    /// 2026-07-28.
    Modern,
    /// 2025-11-25 or 2025-06-18 (the version is only needed by `initialize`).
    Legacy,
}

/// A JSON-RPC error, with the HTTP status the transport answers it with.
#[derive(Debug)]
pub(crate) struct RpcError {
    pub status: StatusCode,
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(status: StatusCode, code: i64, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            data: None,
        }
    }

    fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// `-32602` in a `200`: the request reached the method, which refused its arguments.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(StatusCode::OK, INVALID_PARAMS, message)
    }

    /// `-32601`. A modern server answers it with `404`, which is how a client tells a modern
    /// server from a legacy one; legacy transports carry it in a `200`.
    pub fn method_not_found(era: Era, method: &str) -> Self {
        let status = match era {
            Era::Modern => StatusCode::NOT_FOUND,
            Era::Legacy => StatusCode::OK,
        };
        Self::new(
            status,
            METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        )
    }

    /// A `resources/read` of a URI no route serves, or that the route answered `404`: `-32602`
    /// in 2026-07-28, `-32002` before.
    pub fn resource_not_found(era: Era, uri: &str) -> Self {
        let code = match era {
            Era::Modern => INVALID_PARAMS,
            Era::Legacy => RESOURCE_NOT_FOUND,
        };
        Self::new(StatusCode::OK, code, format!("resource not found: {uri}"))
            .with_data(json!({ "uri": uri }))
    }

    /// A failure of the route behind a resource or a prompt, with its problem as `data`.
    pub fn failed(code: i64, message: impl Into<String>, data: Value) -> Self {
        Self::new(StatusCode::OK, code, message).with_data(data)
    }

    fn header_mismatch(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, HEADER_MISMATCH, message)
    }

    /// `-32022` with the versions this server speaks, which is what lets a client retry.
    pub fn unsupported_version(requested: Option<&str>, legacy: bool) -> Self {
        let supported = supported_versions(legacy);
        Self::new(
            StatusCode::BAD_REQUEST,
            UNSUPPORTED_PROTOCOL_VERSION,
            format!(
                "unsupported protocol version{}; this server supports {}",
                requested.map(|v| format!(" {v}")).unwrap_or_default(),
                supported.join(", ")
            ),
        )
        .with_data(json!({ "supported": supported, "requested": requested }))
    }

    /// The error response. `id` is omitted when it is not known on a modern request (as
    /// 2026-07-28 prescribes) and `null` otherwise (JSON-RPC 2.0).
    pub fn into_response(self, era: Option<Era>, id: Option<&Value>) -> Response {
        let mut error = Map::new();
        error.insert("code".into(), self.code.into());
        error.insert("message".into(), self.message.into());
        if let Some(data) = self.data {
            error.insert("data".into(), data);
        }
        let mut body = Map::new();
        body.insert("jsonrpc".into(), "2.0".into());
        match (id, era) {
            (Some(id), _) => {
                body.insert("id".into(), id.clone());
            }
            (None, Some(Era::Modern)) => {}
            (None, _) => {
                body.insert("id".into(), Value::Null);
            }
        }
        body.insert("error".into(), Value::Object(error));
        json_response(self.status, &Value::Object(body))
    }
}

/// Every version this server answers, modern first.
pub(crate) fn supported_versions(legacy: bool) -> Vec<&'static str> {
    let mut versions = vec![MODERN];
    if legacy {
        versions.extend(LEGACY);
    }
    versions
}

/// A JSON-RPC request as it came in: `id`, `method`, `params` (an object, possibly empty).
#[derive(Debug)]
pub(crate) struct Request {
    pub id: Option<Value>,
    pub method: String,
    pub params: Map<String, Value>,
}

/// What the body of a `POST` turned out to be.
pub(crate) enum Message {
    Request(Request),
    /// No `id`: answered `202` with no body.
    Notification,
}

/// Parse the body of a `POST`: one request or one notification. Batches were removed from the
/// protocol in 2025-06-18; a client must not send responses.
pub(crate) fn parse(body: &[u8]) -> Result<Message, RpcError> {
    let value: Value = serde_json::from_slice(body).map_err(|e| {
        RpcError::new(
            StatusCode::BAD_REQUEST,
            PARSE_ERROR,
            format!("the body is not JSON: {e}"),
        )
    })?;
    let invalid = |message: &str| RpcError::new(StatusCode::BAD_REQUEST, INVALID_REQUEST, message);
    let Value::Object(mut object) = value else {
        return Err(invalid(
            "expected one JSON-RPC request object (batches are not part of MCP)",
        ));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid(r#"expected "jsonrpc": "2.0""#));
    }
    let Some(Value::String(method)) = object.remove("method") else {
        return Err(invalid(
            "expected a request or a notification, with a `method`",
        ));
    };
    let id = match object.remove("id") {
        None => return Ok(Message::Notification),
        Some(id @ (Value::String(_) | Value::Number(_))) => id,
        Some(_) => return Err(invalid("`id` must be a string or a number")),
    };
    let params = match object.remove("params") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(params)) => params,
        Some(_) => {
            return Err(RpcError::new(
                StatusCode::OK,
                INVALID_PARAMS,
                "`params` must be an object",
            ));
        }
    };
    Ok(Message::Request(Request {
        id: Some(id),
        method,
        params,
    }))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// A header that must appear once: a repeated one could be read one way by an intermediary and
/// another way here.
fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, ()> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(());
    }
    Ok(first.and_then(|v| v.to_str().ok()))
}

/// The protocol version a modern request declares in `_meta`, if it declares one.
fn meta_version(params: &Map<String, Value>) -> Option<&str> {
    params.get("_meta")?.get(META_VERSION)?.as_str()
}

/// Choose the era of one request (there is no session to remember it in):
///
/// 1. a version in `_meta` makes it modern, if that version is supported;
/// 2. `initialize` is legacy;
/// 3. otherwise the `MCP-Protocol-Version` header must name a supported legacy version.
///
/// A modern request also has its headers checked against its body here.
pub(crate) fn select_era(
    request: &Request,
    headers: &HeaderMap,
    legacy: bool,
) -> Result<Era, RpcError> {
    if let Some(version) = meta_version(&request.params) {
        if version != MODERN {
            return Err(RpcError::unsupported_version(Some(version), legacy));
        }
        validate_modern_headers(request, headers)?;
        return Ok(Era::Modern);
    }
    let header_version = header(headers, PROTOCOL_VERSION_HEADER);
    if request.method == "initialize" {
        if !legacy {
            let requested = request
                .params
                .get("protocolVersion")
                .and_then(Value::as_str);
            return Err(RpcError::unsupported_version(requested, false));
        }
        return Ok(Era::Legacy);
    }
    match header_version {
        Some(version) if legacy && LEGACY.contains(&version) => Ok(Era::Legacy),
        Some(MODERN) => Err(RpcError::header_mismatch(format!(
            "MCP-Protocol-Version is {MODERN} but `_meta` carries no \"{META_VERSION}\""
        ))),
        Some(version) => Err(RpcError::unsupported_version(Some(version), legacy)),
        None => Err(RpcError::header_mismatch(format!(
            "missing MCP-Protocol-Version header; this server supports {}",
            supported_versions(legacy).join(", ")
        ))),
    }
}

/// The request headers of 2026-07-28 must be present and agree with the body: intermediaries
/// route on them, the server executes the body.
fn validate_modern_headers(request: &Request, headers: &HeaderMap) -> Result<(), RpcError> {
    let expect = |name: &str, label: &str, body: &str| -> Result<(), RpcError> {
        let value = single_header(headers, name)
            .map_err(|()| RpcError::header_mismatch(format!("repeated {label} header")))?;
        match value {
            None => Err(RpcError::header_mismatch(format!("missing {label} header"))),
            Some(value) if decode_header_value(value).as_deref() == Some(body) => Ok(()),
            Some(value) => Err(RpcError::header_mismatch(format!(
                "{label} header value '{value}' does not match body value '{body}'"
            ))),
        }
    };
    expect(PROTOCOL_VERSION_HEADER, "MCP-Protocol-Version", MODERN)?;
    expect(METHOD_HEADER, "Mcp-Method", &request.method)?;
    let name_field = match request.method.as_str() {
        "tools/call" | "prompts/get" => Some("name"),
        "resources/read" => Some("uri"),
        _ => None,
    };
    if let Some(field) = name_field {
        let body = request
            .params
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default();
        expect(NAME_HEADER, "Mcp-Name", body)?;
    }
    Ok(())
}

/// A header value, decoded from the `=?base64?..?=` form clients use for values that are not
/// plain ASCII. `None` when the encoded form is not valid base64 or UTF-8.
fn decode_header_value(value: &str) -> Option<String> {
    match value
        .strip_prefix("=?base64?")
        .and_then(|rest| rest.strip_suffix("?="))
    {
        Some(encoded) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()?;
            String::from_utf8(bytes).ok()
        }
        None => Some(value.to_string()),
    }
}

/// A successful response. Modern results carry `resultType` and the server's identity in
/// `_meta`; legacy results keep the shape of their revision.
pub(crate) fn result_response(
    era: Era,
    id: Option<&Value>,
    mut result: Map<String, Value>,
    server_info: &Value,
) -> Response {
    if era == Era::Modern {
        result.insert("resultType".into(), "complete".into());
        let meta = result
            .entry("_meta")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(meta) = meta {
            meta.insert(META_SERVER_INFO.into(), server_info.clone());
        }
    }
    let body = json!({
        "jsonrpc": "2.0",
        "id": id.cloned().unwrap_or(Value::Null),
        "result": result,
    });
    json_response(StatusCode::OK, &body)
}

/// `initialize`'s version: the client's if it is a legacy version this server speaks, the
/// newest legacy version otherwise (the client then decides whether it can continue).
pub(crate) fn negotiate_legacy(params: &Map<String, Value>) -> &'static str {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    LEGACY
        .iter()
        .find(|v| Some(**v) == requested)
        .copied()
        .unwrap_or(LEGACY[0])
}

pub(crate) fn json_response(status: StatusCode, body: &Value) -> Response {
    let bytes = serde_json::to_vec(body).expect("a JSON value serializes");
    let mut response = (status, Body::from(bytes)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> Request {
        Request {
            id: Some(json!(1)),
            method: method.into(),
            params: params.as_object().cloned().unwrap_or_default(),
        }
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn modern_call(name: &str) -> Request {
        request(
            "tools/call",
            json!({"name": name, "_meta": {META_VERSION: MODERN}}),
        )
    }

    #[test]
    fn modern_requests_need_matching_headers() {
        let ok = headers(&[
            ("mcp-protocol-version", MODERN),
            ("mcp-method", "tools/call"),
            ("mcp-name", "search"),
        ]);
        assert_eq!(
            select_era(&modern_call("search"), &ok, true).unwrap(),
            Era::Modern
        );

        let err = select_era(&modern_call("delete"), &ok, true).unwrap_err();
        assert_eq!(err.code, HEADER_MISMATCH);
        assert_eq!(err.status, StatusCode::BAD_REQUEST);

        let missing = headers(&[
            ("mcp-protocol-version", MODERN),
            ("mcp-method", "tools/call"),
        ]);
        assert_eq!(
            select_era(&modern_call("search"), &missing, true)
                .unwrap_err()
                .message,
            "missing Mcp-Name header"
        );
    }

    #[test]
    fn mcp_name_may_be_base64_encoded() {
        let encoded = headers(&[
            ("mcp-protocol-version", MODERN),
            ("mcp-method", "tools/call"),
            // "héllo"
            ("mcp-name", "=?base64?aMOpbGxv?="),
        ]);
        assert!(select_era(&modern_call("héllo"), &encoded, true).is_ok());
    }

    #[test]
    fn unsupported_modern_versions_list_what_is_supported() {
        let req = request("tools/list", json!({"_meta": {META_VERSION: "2099-01-01"}}));
        let err = select_era(&req, &HeaderMap::new(), true).unwrap_err();
        assert_eq!(err.code, UNSUPPORTED_PROTOCOL_VERSION);
        assert_eq!(
            err.data.unwrap()["supported"],
            json!(["2026-07-28", "2025-11-25", "2025-06-18"])
        );
    }

    #[test]
    fn legacy_requests_are_selected_by_initialize_or_the_header() {
        let init = request("initialize", json!({"protocolVersion": "2025-06-18"}));
        assert_eq!(
            select_era(&init, &HeaderMap::new(), true).unwrap(),
            Era::Legacy
        );
        assert_eq!(negotiate_legacy(&init.params), "2025-06-18");
        let old = request("initialize", json!({"protocolVersion": "2024-11-05"}));
        assert_eq!(negotiate_legacy(&old.params), "2025-11-25");

        let list = request("tools/list", json!({}));
        let legacy = headers(&[("mcp-protocol-version", "2025-11-25")]);
        assert_eq!(select_era(&list, &legacy, true).unwrap(), Era::Legacy);
        assert_eq!(
            select_era(&list, &legacy, false).unwrap_err().code,
            UNSUPPORTED_PROTOCOL_VERSION
        );
        assert_eq!(
            select_era(&list, &HeaderMap::new(), true).unwrap_err().code,
            HEADER_MISMATCH
        );
    }

    #[test]
    fn initialize_without_legacy_names_the_supported_versions() {
        let init = request("initialize", json!({"protocolVersion": "2025-11-25"}));
        let err = select_era(&init, &HeaderMap::new(), false).unwrap_err();
        assert_eq!(err.code, UNSUPPORTED_PROTOCOL_VERSION);
        assert!(err.message.contains("2026-07-28"), "{}", err.message);
    }

    #[test]
    fn parse_refuses_batches_and_accepts_notifications() {
        assert!(matches!(
            parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            Ok(Message::Notification)
        ));
        let Err(err) = parse(br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#) else {
            panic!("a batch must be refused");
        };
        assert_eq!(err.code, INVALID_REQUEST);
        let Err(err) = parse(b"{") else {
            panic!("invalid JSON must be refused");
        };
        assert_eq!(err.code, PARSE_ERROR);
    }
}

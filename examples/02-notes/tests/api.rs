//! End-to-end tests through the router, no port.

use http_body_util::BodyExt;
use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::http::{Method, Request, StatusCode, header};
use notes::{AppState, Tokens, build_app, connect};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn router() -> Router {
    let state = AppState {
        db: lesto::db::Db::new(connect().await),
        tokens: Tokens::demo(),
    };
    build_app().with_state(state).into_router()
}

async fn call(
    router: &Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[lesto::test]
async fn full_crud() {
    let r = router().await;

    let (status, note) = call(
        &r,
        Method::POST,
        "/notes",
        Some("bob-token"),
        Some(json!({"text": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(note["author"], "bob");
    let id = note["id"].as_i64().unwrap();

    let (status, list) = call(&r, Method::GET, "/notes", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, note) = call(
        &r,
        Method::PATCH,
        &format!("/notes/{id}"),
        Some("bob-token"),
        Some(json!({"text": "edited"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{note}");
    assert_eq!(note["text"], "edited");

    let (status, one) = call(&r, Method::GET, &format!("/notes/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["text"], "edited");

    let (status, _) = call(
        &r,
        Method::DELETE,
        &format!("/notes/{id}"),
        Some("alice-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = call(&r, Method::GET, &format!("/notes/{id}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["title"], "Not Found");

    let (status, body) = call(
        &r,
        Method::PATCH,
        &format!("/notes/{id}"),
        Some("bob-token"),
        Some(json!({"text": "too late"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[lesto::test]
async fn authentication_and_permissions() {
    let r = router().await;

    let (status, body) = call(&r, Method::POST, "/notes", None, Some(json!({"text": "x"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["status"], 401);

    let (status, body) = call(
        &r,
        Method::POST,
        "/notes",
        Some("nobody"),
        Some(json!({"text": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["detail"], "Unknown token");

    let (_, note) = call(
        &r,
        Method::POST,
        "/notes",
        Some("bob-token"),
        Some(json!({"text": "mine"})),
    )
    .await;
    let id = note["id"].as_i64().unwrap();

    // bob may write but not delete.
    let (status, body) = call(
        &r,
        Method::DELETE,
        &format!("/notes/{id}"),
        Some("bob-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["required_permission"], "notes:delete");

    // alice may write but is not the author.
    let (status, body) = call(
        &r,
        Method::PATCH,
        &format!("/notes/{id}"),
        Some("alice-token"),
        Some(json!({"text": "hers"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["detail"], "Only the author can edit a note");
}

#[lesto::test]
async fn validation_and_conflicts() {
    let r = router().await;

    let (status, body) = call(
        &r,
        Method::POST,
        "/notes",
        Some("bob-token"),
        Some(json!({"text": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["errors"][0]["pointer"], "/text");

    call(
        &r,
        Method::POST,
        "/notes",
        Some("bob-token"),
        Some(json!({"text": "dup"})),
    )
    .await;
    let (status, body) = call(
        &r,
        Method::POST,
        "/notes",
        Some("bob-token"),
        Some(json!({"text": "dup"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["detail"], "Already exists");

    // PATCH with an empty body changes nothing (all view fields optional).
    let (status, note) = call(
        &r,
        Method::PATCH,
        "/notes/1",
        Some("bob-token"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(note["text"], "dup");
}

#[test]
fn openapi_reflects_the_types() {
    let spec: Value = serde_json::from_str(&build_app().openapi_json()).unwrap();
    assert!(spec["paths"]["/notes"]["get"].get("security").is_none());
    assert_eq!(
        spec["paths"]["/notes"]["post"]["security"],
        json!([{"bearerAuth": []}])
    );
    assert_eq!(
        spec["components"]["schemas"]["NoteCreate"]["required"],
        json!(["text"])
    );
    assert!(
        spec["components"]["schemas"]["NoteUpdate"]
            .get("required")
            .is_none()
    );
    let patch = &spec["paths"]["/notes/{id}"]["patch"];
    assert!(patch["responses"]["403"].is_object());
    assert!(patch["responses"]["404"].is_object());
}

/// An MCP 2026-07-28 request to `/mcp`: version in `_meta`, method and tool name in headers.
async fn mcp(
    router: &Router,
    method: &str,
    params: Value,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut params = params;
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28"});
    let mut req = Request::post("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method);
    if let Some(name) = params.get("name").and_then(Value::as_str) {
        req = req.header("mcp-name", name);
    }
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let res = router
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[lesto::test]
async fn agents_use_the_same_routes_over_mcp() {
    let r = router().await;

    let (_, reply) = mcp(&r, "tools/list", json!({}), None).await;
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    // `delete` is not a tool.
    assert_eq!(
        names,
        ["list_notes", "get_note", "create_note", "update_note"]
    );

    // Writing needs the token: the route's 401 is the MCP response's.
    let create = json!({"name": "create_note", "arguments": {"text": "from an agent"}});
    let (status, _) = mcp(&r, "tools/call", create.clone(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, reply) = mcp(&r, "tools/call", create, Some("bob-token")).await;
    assert_eq!(status, StatusCode::OK);
    let note = &reply["result"]["structuredContent"];
    assert_eq!(note["author"], "bob");
    let id = note["id"].as_i64().unwrap();

    // Path parameter and body properties side by side.
    let update = json!({"name": "update_note", "arguments": {"id": id, "text": ""}});
    let (_, reply) = mcp(&r, "tools/call", update, Some("bob-token")).await;
    assert_eq!(reply["result"]["isError"], true, "{reply}");

    let (_, list) = call(&r, Method::GET, "/notes", None, None).await;
    assert_eq!(list[0]["text"], "from an agent");
}

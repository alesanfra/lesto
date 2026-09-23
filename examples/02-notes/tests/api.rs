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

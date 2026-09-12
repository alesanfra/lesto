//! API Gateway (v1 with stage, v2), Function URL and ALB events through a lesto app.

use lesto::http::StatusCode;
use lesto::lambda::Options;
use lesto::lambda::test::{invoke, invoke_with};
use lesto::prelude::*;
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, JsonSchema, Validate)]
struct Note {
    #[garde(skip)]
    id: u64,
    #[garde(length(min = 1))]
    text: String,
}

/// Read a note.
#[lesto::get("/notes/{id}")]
async fn get_note(Path(id): Path<u64>) -> Result<Json<Note>, HttpError> {
    if id == 7 {
        Ok(Json(Note {
            id,
            text: "seven".into(),
        }))
    } else {
        Err(HttpError::not_found(format!("note {id} not found")))
    }
}

/// Create a note.
#[lesto::post("/notes", status = 201)]
async fn create_note(Json(note): Json<Note>) -> Json<Note> {
    Json(note)
}

fn router() -> lesto::axum::Router {
    App::<()>::new()
        .title("Lambda notes")
        .routes(routes![get_note, create_note])
        .into_router()
}

fn body_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into()))
}

/// HTTP API / Function URL event (payload format 2.0).
fn v2_event(method: &str, path: &str, body: Option<Value>) -> String {
    json!({
        "version": "2.0",
        "routeKey": "$default",
        "rawPath": path,
        "rawQueryString": "",
        "headers": {
            "host": "abc.execute-api.eu-west-1.amazonaws.com",
            "content-type": "application/json",
            "x-forwarded-proto": "https"
        },
        "requestContext": {
            "accountId": "123456789012",
            "apiId": "abc",
            "domainName": "abc.execute-api.eu-west-1.amazonaws.com",
            "domainPrefix": "abc",
            "http": {
                "method": method,
                "path": path,
                "protocol": "HTTP/1.1",
                "sourceIp": "203.0.113.1",
                "userAgent": "curl"
            },
            "requestId": "id",
            "routeKey": "$default",
            "stage": "$default",
            "time": "12/Sep/2026:10:00:00 +0000",
            "timeEpoch": 1789000000000u64
        },
        "body": body.map(|b| b.to_string()),
        "isBase64Encoded": false
    })
    .to_string()
}

/// REST API event (payload format 1.0) deployed to stage `prod`.
fn v1_event(method: &str, path: &str, body: Option<Value>) -> String {
    json!({
        "resource": "/{proxy+}",
        "path": path,
        "httpMethod": method,
        "headers": {"Host": "abc.execute-api.eu-west-1.amazonaws.com", "Content-Type": "application/json"},
        "multiValueHeaders": {"Host": ["abc.execute-api.eu-west-1.amazonaws.com"], "Content-Type": ["application/json"]},
        "queryStringParameters": null,
        "multiValueQueryStringParameters": null,
        "pathParameters": {"proxy": path.trim_start_matches('/')},
        "stageVariables": null,
        "requestContext": {
            "accountId": "123456789012",
            "apiId": "abc",
            "stage": "prod",
            "requestId": "id",
            "identity": {"sourceIp": "203.0.113.1", "userAgent": "curl"},
            "resourcePath": "/{proxy+}",
            "httpMethod": method,
            "path": format!("/prod{path}"),
            "protocol": "HTTP/1.1",
            "requestTimeEpoch": 1789000000000u64
        },
        "body": body.map(|b| b.to_string()),
        "isBase64Encoded": false
    })
    .to_string()
}

/// Application Load Balancer target event.
fn alb_event(method: &str, path: &str) -> String {
    json!({
        "requestContext": {"elb": {"targetGroupArn": "arn:aws:elasticloadbalancing:eu-west-1:123456789012:targetgroup/x/abc"}},
        "httpMethod": method,
        "path": path,
        "queryStringParameters": {},
        "headers": {"host": "alb.example.test", "x-forwarded-proto": "https"},
        "body": "",
        "isBase64Encoded": false
    })
    .to_string()
}

#[tokio::test]
async fn http_api_v2_get() {
    let res = invoke(&router(), &v2_event("GET", "/notes/7", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.body()), json!({"id": 7, "text": "seven"}));
}

#[tokio::test]
async fn rest_api_v1_stage_is_stripped_and_instance_is_the_route_path() {
    let res = invoke(&router(), &v1_event("GET", "/notes/99", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(res.headers()["content-type"], "application/problem+json");
    let body = body_json(res.body());
    assert_eq!(body["detail"], "note 99 not found");
    assert_eq!(body["instance"], "/notes/99", "stage removed");
}

#[tokio::test]
async fn validation_errors_travel_through_lambda() {
    let res = invoke(
        &router(),
        &v2_event("POST", "/notes", Some(json!({"id": 1, "text": ""}))),
    )
    .await
    .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(res.body());
    assert_eq!(body["errors"][0]["pointer"], "/text");

    let res = invoke(
        &router(),
        &v1_event("POST", "/notes", Some(json!({"id": 1, "text": "ok"}))),
    )
    .await
    .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(body_json(res.body())["text"], "ok");
}

#[tokio::test]
async fn alb_event_is_served() {
    let res = invoke(&router(), &alb_event("GET", "/notes/7"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res.body())["id"], 7);
}

#[tokio::test]
async fn docs_work_behind_a_stage() {
    // `/prod/docs` reaches `/docs` and the page links `openapi.json` relatively, which the
    // browser resolves to `/prod/openapi.json`.
    let res = invoke(&router(), &v1_event("GET", "/docs", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = String::from_utf8_lossy(res.body());
    assert!(html.contains(r#"url: "openapi.json""#));
    let res = invoke(&router(), &v1_event("GET", "/openapi.json", None))
        .await
        .unwrap();
    assert_eq!(body_json(res.body())["info"]["title"], "Lambda notes");
}

#[test]
fn not_in_lambda_here() {
    assert!(!lesto::lambda::in_lambda());
    assert!(!lesto::lambda::Options::default().keep_stage);
}

#[lesto::get("/prod/health")]
async fn staged_health() -> &'static str {
    "ok"
}

#[tokio::test]
async fn keep_stage_is_a_per_router_option() {
    // `keep_stage` leaves `/prod` in the path, so a route declared with the stage matches...
    let staged = App::<()>::new()
        .routes(routes![staged_health])
        .into_router();
    let keep = Options { keep_stage: true };
    let res = invoke_with(&staged, &v1_event("GET", "/health", None), keep)
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.body().as_ref(), b"ok");
    // ...while the default still strips it, in the same process, right after.
    let res = invoke(&router(), &v1_event("GET", "/notes/7", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // A v2 event with a named stage is stripped the same way.
    let res = invoke(&router(), &v2_event_with_stage("beta", "GET", "/notes/7"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{:?}", body_json(res.body()));
}

/// HTTP API event deployed to a named stage: the stage is part of `rawPath`.
fn v2_event_with_stage(stage: &str, method: &str, path: &str) -> String {
    let mut event: Value = serde_json::from_str(&v2_event(method, path, None)).unwrap();
    event["rawPath"] = json!(format!("/{stage}{path}"));
    event["requestContext"]["stage"] = json!(stage);
    event["requestContext"]["http"]["path"] = json!(format!("/{stage}{path}"));
    event.to_string()
}

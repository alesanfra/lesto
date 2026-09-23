# 12. Testing

An `App` turns into a tower service you can call directly, without opening a port. It is the
standard way of testing axum, and therefore lesto.

## Test dependencies

```toml
[dev-dependencies]
tower = { version = "0.5", features = ["util"] }
http-body-util = "0.1"
serde_json = "1"
```

## A test

```rust
use http_body_util::BodyExt;
use lesto::axum::body::Body;
use lesto::http::{header, Request, StatusCode};
use lesto::prelude::*;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::{build_app, AppState};

async fn call(app: App<()>, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.into_router().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[lesto::test]
async fn creates_a_user() {
    let app = build_app(AppState::for_tests());
    let req = Request::post("/users")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"name": "Ada", "password": "correct horse"}).to_string()))
        .unwrap();

    let (status, body) = call(app, req).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["name"], "Ada");
    assert!(body.get("password").is_none());
}

#[lesto::test]
async fn rejects_short_passwords() {
    let app = build_app(AppState::for_tests());
    let req = Request::post("/users")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"name": "Ada", "password": "x"}).to_string()))
        .unwrap();

    let (status, body) = call(app, req).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["errors"][0]["pointer"], "/password");
}
```

`oneshot` consumes the router: build a fresh `App` for each request (it is cheap) or keep the
`Router` and use `.clone().oneshot(...)`.

## Testing the documentation

`App::openapi()` returns the document without starting anything. Use it to make sure a refactor
does not change the contract:

```rust
#[test]
fn openapi_contract() {
    let spec = serde_json::to_value(build_app(AppState::for_tests()).openapi()).unwrap();
    assert!(spec["paths"]["/users"]["post"].is_object());
    assert_eq!(spec["paths"]["/users"]["post"]["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
               "#/components/schemas/UserOut");
}
```

A useful pattern is to save `openapi_json()` to a versioned file and compare it in tests
(snapshot): every contract change becomes visible in code review.

## Testing a handler directly

Handlers remain ordinary functions: you can call them without HTTP.

```rust
#[lesto::test]
async fn health_says_ok() {
    assert_eq!(health().await, "ok");
}
```

For handlers with extractors, build the values by hand: `Json(UserIn { .. })`, `Path(42)`,
`State(state)`.

## Recap

- `app.into_router().oneshot(request)` for in-memory end-to-end tests.
- `app.openapi()` to test the contract.
- Handlers are functions: test them directly when that is enough.

Next: [Databases: stores and principals](13-databases.md).

Appendices: [From FastAPI to lesto](A-from-fastapi-to-lesto.md), [Common problems](B-common-problems.md),
[Why axum and not actix-web](C-why-axum.md).

//! Both APIs through one router, no port.

use http_body_util::BodyExt;
use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::http::{Method, Request, StatusCode, header};
use routers::{AppState, build_app};
use serde_json::{Value, json};
use tower::ServiceExt;

fn router() -> Router {
    build_app().with_state(AppState::default()).into_router()
}

async fn call(
    router: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(uri);
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

#[tokio::test]
async fn orders_placed_in_the_app_api_show_up_in_analytics() {
    let r = router();

    for (name, price) in [("Espresso", 120), ("Cornetto", 150)] {
        let (status, _) = call(
            &r,
            Method::POST,
            "/api/app/v1/products",
            Some(json!({"name": name, "price_cents": price})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    for (product_id, quantity) in [(1, 3), (2, 1), (1, 1)] {
        let (status, order) = call(
            &r,
            Method::POST,
            "/api/app/v1/orders",
            Some(json!({"product_id": product_id, "quantity": quantity})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{order}");
    }

    let (status, list) = call(&r, Method::GET, "/api/app/v1/products", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 2);

    let (status, sales) = call(&r, Method::GET, "/api/analytics/v1/sales", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sales,
        json!({"orders": 3, "items_sold": 5, "revenue_cents": 630})
    );

    let (_, top) = call(
        &r,
        Method::GET,
        "/api/analytics/v1/products/top?limit=1",
        None,
    )
    .await;
    assert_eq!(
        top,
        json!([{"product_id": 1, "name": "Espresso", "items_sold": 4, "revenue_cents": 480}])
    );
}

#[tokio::test]
async fn errors_are_problems_under_either_prefix() {
    let r = router();

    let (status, body) = call(&r, Method::GET, "/api/app/v1/products/9", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["instance"], "/api/app/v1/products/9");

    let (status, _) = call(
        &r,
        Method::POST,
        "/api/app/v1/orders",
        Some(json!({"product_id": 9, "quantity": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = call(
        &r,
        Method::GET,
        "/api/analytics/v1/products/top?limit=0",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The APIs are separate: analytics routes do not exist under the app prefix.
    let (status, _) = call(&r, Method::GET, "/api/app/v1/sales", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_document_with_the_full_paths() {
    let (status, doc) = call(&router(), Method::GET, "/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);

    let mut paths: Vec<&str> = doc["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "/api/analytics/v1/products/top",
            "/api/analytics/v1/sales",
            "/api/app/v1/orders",
            "/api/app/v1/products",
            "/api/app/v1/products/{id}",
        ]
    );

    let tags: Vec<&str> = doc["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tags, ["products", "orders", "analytics"]);

    // operationIds come from the final path, so `create` in two modules does not collide.
    assert_eq!(
        doc["paths"]["/api/app/v1/orders"]["post"]["operationId"],
        "create_api_app_v1_orders_post"
    );
    assert_eq!(
        doc["paths"]["/api/app/v1/products"]["post"]["operationId"],
        "create_api_app_v1_products_post"
    );
}

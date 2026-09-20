//! Per-request framework overhead: lesto against plain axum, through `tower::Service` calls.
//!
//! `cargo bench -p lesto`. No networking, no throughput: the numbers are the cost of the
//! framework itself around a handler that does nothing, which is what the layers in
//! `into_router` are paid out of.
//!
//! The harness is hand-written (`harness = false`, no dev-dependency): every case is warmed up,
//! then timed over several rounds, and the best round is reported. A best-of is the stable
//! statistic on a shared machine, where every disturbance only ever makes a round slower.
//! `LESTO_BENCH_ITERS` (default 200000) and `LESTO_BENCH_ROUNDS` (default 5) override the size.
//!
//! The axum line of a case is the same request through a plain router, which is the comparison
//! the goal is stated in — not the same *answer*: plain axum does not validate, so the invalid
//! body is a `201` there and a `422` here. The status and body size printed after each number
//! say which.

use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lesto::prelude::*;
use tower::{Service, ServiceExt};

#[derive(Deserialize, Serialize, JsonSchema, Validate)]
struct Item {
    #[garde(length(min = 1))]
    name: String,
    #[garde(range(min = 0))]
    qty: i64,
}

#[lesto::get("/hello")]
async fn hello() -> &'static str {
    "hi"
}

#[lesto::post("/items", status = 201)]
async fn create(Json(item): Json<Item>) -> Json<Item> {
    Json(item)
}

#[lesto::get("/err", responses(404))]
async fn err() -> Result<Json<Item>, HttpError> {
    Err(HttpError::not_found("no such item"))
}

async fn axum_hello() -> &'static str {
    "hi"
}

async fn axum_create(axum::Json(item): axum::Json<Item>) -> (StatusCode, axum::Json<Item>) {
    (StatusCode::CREATED, axum::Json(item))
}

async fn axum_err() -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({"detail": "no such item"})),
    )
}

/// The cases. A `Request` is consumed by the call, so each iteration builds its own.
#[derive(Clone, Copy)]
enum Case {
    Hello,
    Create,
    CreateInvalid,
    Missing,
    Err,
}

impl Case {
    fn request(self) -> Request<Body> {
        match self {
            Case::Hello => Request::get("/hello").body(Body::empty()).unwrap(),
            Case::Create => Request::post("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"abc","qty":3}"#))
                .unwrap(),
            Case::CreateInvalid => Request::post("/items")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"","qty":-1}"#))
                .unwrap(),
            Case::Missing => Request::get("/missing").body(Body::empty()).unwrap(),
            Case::Err => Request::get("/err").body(Body::empty()).unwrap(),
        }
    }
}

fn axum_router() -> axum::Router {
    axum::Router::new()
        .route("/hello", axum::routing::get(axum_hello))
        .route("/items", axum::routing::post(axum_create))
        .route("/err", axum::routing::get(axum_err))
}

fn lesto_router(trace: Trace) -> axum::Router {
    App::new()
        .trace(trace)
        .routes(routes![hello, create, err])
        .into_router()
}

async fn call(svc: &mut axum::Router, case: Case) -> (StatusCode, usize) {
    let response = ServiceExt::<Request<Body>>::ready(svc)
        .await
        .unwrap()
        .call(case.request())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (parts.status, bytes.len())
}

async fn run(name: &str, mut svc: axum::Router, case: Case, iters: u32, rounds: u32) {
    // Warm up the allocator and the branch predictors; the first answer also labels the line,
    // so a case that silently stopped doing what it claims is visible in the output.
    let (status, len) = call(&mut svc, case).await;
    for _ in 0..iters / 10 {
        call(&mut svc, case).await;
    }

    let mut best = f64::MAX;
    for _ in 0..rounds {
        let start = Instant::now();
        for _ in 0..iters {
            call(&mut svc, case).await;
        }
        let per_request = start.elapsed().as_nanos() as f64 / f64::from(iters);
        best = best.min(per_request);
    }
    println!(
        "{name:<52} {best:8.0} ns/req   [{} {len} B]",
        status.as_u16()
    );
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let iters = env_u32("LESTO_BENCH_ITERS", 200_000);
    let rounds = env_u32("LESTO_BENCH_ROUNDS", 5);
    let default = || lesto_router(Trace::new());
    let quiet = || lesto_router(Trace::off());

    for (case, label) in [
        (Case::Hello, "GET /hello"),
        (Case::Create, "POST /items json+validate 201"),
        (Case::CreateInvalid, "POST /items invalid 422"),
        (Case::Missing, "GET /missing 404"),
        (Case::Err, "GET /err handler HttpError 404"),
    ] {
        run(
            &format!("axum            {label}"),
            axum_router(),
            case,
            iters,
            rounds,
        )
        .await;
        run(
            &format!("lesto           {label}"),
            default(),
            case,
            iters,
            rounds,
        )
        .await;
        run(
            &format!("lesto trace off {label}"),
            quiet(),
            case,
            iters,
            rounds,
        )
        .await;
        println!();
    }
}

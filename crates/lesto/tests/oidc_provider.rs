//! `lesto::oidc` against a real OpenID Connect provider: `mock-oauth2-server`, whose discovery
//! document, key set and tokens lesto did not write. `tests/oidc.rs` covers every check with a
//! provider built for the purpose; this file checks the half it cannot — that a provider in the
//! wild agrees with it (issuer with a path, `aud` as a list, `nbf`, pretty-printed JSON, its own
//! `kid`s).
//!
//! Needs the provider, so it is skipped rather than run when its environment is missing:
//!
//! ```sh
//! (cd examples/06-oidc && docker compose up -d)
//! LESTO_TEST_OIDC_URL=http://localhost:8089/default cargo test -p lesto --test oidc_provider
//! ```
//!
//! The provider's claims come from its `JSON_CONFIG`, which `examples/06-oidc/compose.yaml` and
//! the `oauth2` service of `.github/workflows/ci.yml` both set: keep the two in sync.

use std::time::Duration;

use http_body_util::BodyExt;
use lesto::axum::body::Body;
use lesto::http::{Request, StatusCode, header};
use lesto::oidc::{Jwt, Oidc};
use lesto::prelude::*;
use serde_json::Value;
use tower::ServiceExt;

/// The issuer URL, e.g. `http://localhost:8089/default`, or `None` to skip.
fn issuer() -> Option<String> {
    let url = std::env::var("LESTO_TEST_OIDC_URL").ok()?;
    Some(url.trim_end_matches('/').to_string())
}

/// Waits for the provider (a CI service may still be starting), then discovers it.
async fn discover(issuer: &str) -> Oidc {
    let url = format!("{issuer}/.well-known/openid-configuration");
    let mut last = None;
    for _ in 0..60 {
        match Oidc::discover(&url)
            .audiences(["api://notes"])
            .clients(["notes-cli"])
            .await
        {
            Ok(oidc) => return oidc,
            Err(error) => last = Some(error),
        }
        lesto::tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("the provider at {url} did not answer: {last:?}");
}

/// A client-credentials token; the scope picks the claims (see `JSON_CONFIG`).
async fn token(issuer: &str, scope: &str) -> String {
    let response: Value = reqwest::Client::new()
        .post(format!("{issuer}/token"))
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", "notes-cli"),
            ("client_secret", "secret"),
            ("scope", scope),
        ])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    response["access_token"].as_str().unwrap().to_string()
}

#[lesto::get("/health")]
async fn health() -> &'static str {
    "ok"
}

#[lesto::get("/me")]
async fn me(token: Jwt) -> String {
    format!(
        "{} ({})",
        token.subject().unwrap_or("?"),
        token.scopes().collect::<Vec<_>>().join(" ")
    )
}

#[lesto::get("/stats")]
async fn stats() -> &'static str {
    "42 notes"
}

/// `examples/06-oidc`'s app.
fn app(auth: Oidc) -> App {
    let admin = App::new()
        .routes(routes![stats])
        .protect(auth.clone().scopes(["admin"]));
    App::new()
        .oidc(auth)
        .routes(routes![health, me])
        .nest("/admin", admin)
}

struct Answer {
    status: StatusCode,
    challenge: Option<String>,
    body: String,
}

async fn call(router: &lesto::axum::Router, path: &str, token: Option<&str>) -> Answer {
    let mut request = Request::get(path);
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        challenge,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

#[lesto::test]
async fn a_real_provider_agrees() {
    let Some(issuer) = issuer() else {
        eprintln!("LESTO_TEST_OIDC_URL is not set: skipping the real-provider test");
        return;
    };
    let auth = discover(&issuer).await;
    let app = app(auth);
    let spec: Value = serde_json::from_str(&app.openapi_json()).unwrap();
    let router = app.into_router();
    let reader = token(&issuer, "notes:read").await;
    let admin = token(&issuer, "admin").await;

    // A `Jwt` argument.
    assert_eq!(call(&router, "/health", None).await.status, StatusCode::OK);
    let answer = call(&router, "/me", None).await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert_eq!(answer.challenge.as_deref(), Some("Bearer"));
    let answer = call(&router, "/me", Some(&reader)).await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (StatusCode::OK, "bob (notes:read)")
    );

    // A token whose signature is not the provider's: the payload of another token, the
    // signature of this one.
    let [_, reader_payload, _]: [&str; 3] =
        reader.split('.').collect::<Vec<_>>().try_into().unwrap();
    let [admin_header, _, admin_signature]: [&str; 3] =
        admin.split('.').collect::<Vec<_>>().try_into().unwrap();
    let forged = format!("{admin_header}.{reader_payload}.{admin_signature}");
    let answer = call(&router, "/me", Some(&forged)).await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert!(
        answer
            .challenge
            .as_deref()
            .is_some_and(|c| c.starts_with("Bearer error=\"invalid_token\"")),
        "{:?}",
        answer.challenge
    );

    // The protected app.
    assert_eq!(
        call(&router, "/admin/stats", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let answer = call(&router, "/admin/stats", Some(&reader)).await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN);
    assert_eq!(
        answer.challenge.as_deref(),
        Some("Bearer error=\"insufficient_scope\", scope=\"admin\"")
    );
    let answer = call(&router, "/admin/stats", Some(&admin)).await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (StatusCode::OK, "42 notes")
    );

    // The document: the provider's discovery URL, security only where a token is needed.
    assert_eq!(
        spec["components"]["securitySchemes"]["openIdConnect"]["openIdConnectUrl"],
        format!("{issuer}/.well-known/openid-configuration")
    );
    assert!(spec["paths"]["/health"]["get"].get("security").is_none());
    assert_eq!(
        spec["paths"]["/me"]["get"]["security"],
        serde_json::json!([{"openIdConnect": []}])
    );
    assert_eq!(
        spec["paths"]["/admin/stats"]["get"]["security"],
        serde_json::json!([{"openIdConnect": ["admin"]}])
    );
}

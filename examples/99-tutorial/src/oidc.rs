// Chapter 9: OpenID Connect, verified tokens (feature `oidc`)
use lesto::oidc::{Jwt, Oidc, StandardClaims};
use lesto::prelude::*;

/// Who is calling, according to the token.
#[lesto::get("/me")]
async fn me(token: Jwt) -> Json<StandardClaims> {
    Json(token.into_claims())
}

#[lesto::model]
struct Claims {
    sub: String,
    email: String,
}

#[lesto::get("/email")]
async fn my_email(token: Jwt<Claims>) -> String {
    token.claims().email.clone()
}

#[lesto::get("/health")]
async fn health() -> &'static str {
    "ok"
}

#[lesto::get("/stats")]
async fn stats() -> &'static str {
    "stats"
}

#[lesto::delete("/cache", status = 204)]
async fn purge() {}

/// The chapter's `main`, minus the serving: discovery, then the app.
#[allow(dead_code)]
pub async fn discover_and_build() -> Result<App, Box<dyn std::error::Error>> {
    let auth = Oidc::discover("https://login.example.com/.well-known/openid-configuration")
        .audiences(["api://notes"]) // optional: `aud` must contain one of these
        .clients(["web", "cli"]) // optional: the token must be issued to one of these
        .await?;
    Ok(App::new().oidc(auth).routes(routes![me]))
}

/// "Protecting a group of routes".
#[allow(dead_code)]
pub fn protected_app(auth: Oidc) -> App {
    let admin = App::new()
        .routes(routes![stats, purge])
        .protect(auth.clone().scopes(["admin"])); // or `.protect(auth.clone())`: any valid token

    App::new()
        .oidc(auth)
        .routes(routes![health, me, my_email])
        .nest("/admin", admin)
}

/// "From the environment".
#[allow(dead_code)]
pub async fn from_env(app: App) -> Result<App, lesto::oidc::Error> {
    let auth = Oidc::from_env().await?; // Option<Oidc>
    let app = match auth {
        Some(auth) => app.oidc(auth),
        None => app,
    };
    Ok(app)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use http_body_util::BodyExt;
    use jsonwebtoken::jwk::{Jwk, JwkSet};
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    use lesto::axum::body::Body;
    use lesto::http::{Request, StatusCode, header};
    use lesto::serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    const KEY: &[u8] = include_bytes!("../../../crates/lesto/tests/fixtures/oidc/rsa-1.pem");

    /// A provider on `127.0.0.1:0`: the discovery document and the key set. Never a real one.
    async fn provider(jwk: Jwk) -> String {
        let listener = lesto::tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let discovery = json!({"issuer": base, "jwks_uri": format!("{base}/jwks")});
        let jwks = lesto::serde_json::to_value(JwkSet { keys: vec![jwk] }).unwrap();
        let router = lesto::axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                lesto::axum::routing::get(move || async move { lesto::axum::Json(discovery) }),
            )
            .route(
                "/jwks",
                lesto::axum::routing::get(move || async move { lesto::axum::Json(jwks) }),
            );
        lesto::tokio::spawn(async move { lesto::axum::serve(listener, router).await });
        base
    }

    fn sign(key: &EncodingKey, claims: Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("k1".into());
        jsonwebtoken::encode(&header, &claims, key).unwrap()
    }

    async fn call(router: &lesto::axum::Router, path: &str, token: &str) -> (StatusCode, String) {
        let mut request = Request::get(path);
        if !token.is_empty() {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[lesto::test]
    async fn the_chapter_9_claims_hold() {
        let key = EncodingKey::from_rsa_pem(KEY).unwrap();
        let mut jwk = Jwk::from_encoding_key(&key, Algorithm::RS256).unwrap();
        jwk.common.key_id = Some("k1".into());
        let base = provider(jwk).await;
        let auth = Oidc::discover(format!("{base}/.well-known/openid-configuration"))
            .audiences(["api://notes"])
            .clients(["web", "cli"])
            .await
            .unwrap();
        let router = protected_app(auth).into_router();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims = json!({
            "iss": base, "sub": "ada", "aud": "api://notes", "azp": "web",
            "exp": now + 300, "email": "ada@example.com", "scope": "notes:read",
        });
        let token = sign(&key, claims.clone());

        // A `Jwt` argument: 401 without a token, the claims with one.
        assert_eq!(call(&router, "/me", "").await.0, StatusCode::UNAUTHORIZED);
        let (status, body) = call(&router, "/email", &token).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "ada@example.com"));

        // An expired token: 401 naming the class.
        let mut expired = claims.clone();
        expired["exp"] = json!(now - 120);
        let (status, body) = call(&router, "/me", &sign(&key, expired)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("The token has expired"), "{body}");

        // The protected group: public next to it, 403 without the scope, 200 with it.
        assert_eq!(call(&router, "/health", "").await.0, StatusCode::OK);
        assert_eq!(
            call(&router, "/admin/stats", &token).await.0,
            StatusCode::FORBIDDEN
        );
        let mut admin = claims;
        admin["scope"] = json!("admin");
        assert_eq!(
            call(&router, "/admin/stats", &sign(&key, admin)).await.0,
            StatusCode::OK
        );
    }
}

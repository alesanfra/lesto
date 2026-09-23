//! `lesto::oidc` end to end: a provider on `127.0.0.1:0` serving a discovery document and a key
//! set, tokens signed with the keys in `tests/fixtures/oidc`, requests through `oneshot`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use http_body_util::BodyExt;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::axum::extract::State as AxumState;
use lesto::axum::response::IntoResponse;
use lesto::axum::routing::get;
use lesto::http::{Request, StatusCode, header};
use lesto::oidc::{Error, Jwt, Oidc, StandardClaims};
use lesto::prelude::*;
use serde_json::{Value, json};
use tower::ServiceExt;

const RSA_1: &[u8] = include_bytes!("fixtures/oidc/rsa-1.pem");
const RSA_2: &[u8] = include_bytes!("fixtures/oidc/rsa-2.pem");
const EC: &[u8] = include_bytes!("fixtures/oidc/ec.pem");

// ---- the provider ---------------------------------------------------------------------------

/// A signing key and its public half, as the provider publishes it.
struct Key {
    kid: &'static str,
    alg: Algorithm,
    encoding: EncodingKey,
    jwk: Jwk,
}

impl Key {
    fn rsa(kid: &'static str, pem: &[u8]) -> Key {
        let encoding = EncodingKey::from_rsa_pem(pem).unwrap();
        Key::new(kid, Algorithm::RS256, encoding)
    }

    fn ec(kid: &'static str) -> Key {
        let encoding = EncodingKey::from_ec_pem(EC).unwrap();
        Key::new(kid, Algorithm::ES256, encoding)
    }

    fn new(kid: &'static str, alg: Algorithm, encoding: EncodingKey) -> Key {
        let mut jwk = Jwk::from_encoding_key(&encoding, alg).unwrap();
        jwk.common.key_id = Some(kid.to_string());
        Key {
            kid,
            alg,
            encoding,
            jwk,
        }
    }

    fn sign(&self, claims: &Value) -> String {
        let mut header = Header::new(self.alg);
        header.kid = Some(self.kid.to_string());
        jsonwebtoken::encode(&header, claims, &self.encoding).unwrap()
    }
}

#[derive(Clone)]
struct Provider {
    issuer: Arc<Mutex<String>>,
    jwks: Arc<Mutex<Value>>,
    /// `None`: 200 with the set; `Some(status)`: that status.
    jwks_status: Arc<Mutex<Option<StatusCode>>>,
    jwks_fetches: Arc<AtomicUsize>,
    algorithms: Arc<Mutex<Value>>,
    base: String,
}

impl Provider {
    async fn start(keys: &[&Key]) -> Provider {
        let listener = lesto::tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let provider = Provider {
            issuer: Arc::new(Mutex::new(base.clone())),
            jwks: Arc::new(Mutex::new(Value::Null)),
            jwks_status: Arc::new(Mutex::new(None)),
            jwks_fetches: Arc::new(AtomicUsize::new(0)),
            algorithms: Arc::new(Mutex::new(json!(["RS256", "ES256"]))),
            base,
        };
        provider.publish(keys);
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .with_state(provider.clone());
        lesto::tokio::spawn(async move { lesto::axum::serve(listener, router).await });
        provider
    }

    fn publish(&self, keys: &[&Key]) {
        let set = JwkSet {
            keys: keys.iter().map(|k| k.jwk.clone()).collect(),
        };
        *self.jwks.lock().unwrap() = serde_json::to_value(set).unwrap();
    }

    fn discovery_url(&self) -> String {
        format!("{}/.well-known/openid-configuration", self.base)
    }

    fn issuer(&self) -> String {
        self.issuer.lock().unwrap().clone()
    }

    fn fetches(&self) -> usize {
        self.jwks_fetches.load(Ordering::SeqCst)
    }
}

async fn discovery(AxumState(p): AxumState<Provider>) -> impl IntoResponse {
    lesto::axum::Json(json!({
        "issuer": p.issuer(),
        "jwks_uri": format!("{}/jwks", p.base),
        "id_token_signing_alg_values_supported": p.algorithms.lock().unwrap().clone(),
    }))
}

async fn jwks(AxumState(p): AxumState<Provider>) -> lesto::axum::response::Response {
    p.jwks_fetches.fetch_add(1, Ordering::SeqCst);
    if let Some(status) = *p.jwks_status.lock().unwrap() {
        return status.into_response();
    }
    lesto::axum::Json(p.jwks.lock().unwrap().clone()).into_response()
}

// ---- tokens ---------------------------------------------------------------------------------

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Valid claims for `provider`, with `extra` merged over them.
fn claims(provider: &Provider, extra: Value) -> Value {
    let mut claims = json!({
        "iss": provider.issuer(),
        "sub": "user-1",
        "aud": "api://notes",
        "azp": "web",
        "exp": now() + 300,
        "iat": now(),
        "scope": "notes:read",
    });
    for (k, v) in extra.as_object().unwrap() {
        if v.is_null() {
            claims.as_object_mut().unwrap().remove(k);
        } else {
            claims[k] = v.clone();
        }
    }
    claims
}

fn b64(value: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

// ---- the app --------------------------------------------------------------------------------

#[lesto::get("/me")]
async fn me(token: Jwt) -> Json<StandardClaims> {
    Json(token.into_claims())
}

#[derive(Deserialize)]
struct Email {
    email: String,
}

#[lesto::get("/email")]
async fn email(token: Jwt<Email>) -> String {
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

#[lesto::get("/whoami")]
async fn whoami(token: Jwt) -> String {
    token.subject().unwrap_or_default().to_string()
}

async fn oidc(provider: &Provider) -> Oidc {
    Oidc::discover(provider.discovery_url())
        .audiences(["api://notes"])
        .clients(["web", "cli"])
        .await
        .unwrap()
}

fn app(oidc: Oidc) -> Router {
    App::new()
        .oidc(oidc)
        .routes(routes![me, email])
        .into_router()
}

async fn call(router: &Router, path: &str, token: Option<&str>) -> (StatusCode, String, Value) {
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
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, challenge, body)
}

/// Assert a `401` problem whose detail names `class`.
async fn refused(router: &Router, token: &str, detail: &str) {
    let (status, challenge, body) = call(router, "/me", Some(token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["detail"], detail, "{body}");
    assert!(
        challenge.starts_with("Bearer error=\"invalid_token\""),
        "{challenge}"
    );
}

// ---- verification ---------------------------------------------------------------------------

#[lesto::test]
async fn a_valid_token_reaches_the_handler() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = app(oidc(&provider).await);

    let token = key.sign(&claims(&provider, json!({"email": "a@example.com"})));
    let (status, _, body) = call(&router, "/me", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sub"], "user-1");
    assert_eq!(body["aud"], json!(["api://notes"]));
    assert_eq!(body["email"], "a@example.com");

    let (status, _, body) = call(&router, "/email", Some(&token)).await;
    assert_eq!((status, body), (StatusCode::OK, json!("a@example.com")));
}

#[lesto::test]
async fn an_ec_key_verifies_too() {
    let rsa = Key::rsa("k1", RSA_1);
    let ec = Key::ec("e1");
    let provider = Provider::start(&[&rsa, &ec]).await;
    let router = app(oidc(&provider).await);
    let (status, _, body) = call(
        &router,
        "/me",
        Some(&ec.sign(&claims(&provider, json!({})))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[lesto::test]
async fn a_missing_token_gets_a_bare_challenge() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = app(oidc(&provider).await);
    let (status, challenge, body) = call(&router, "/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge, "Bearer");
    assert_eq!(body["detail"], "Not authenticated");
}

#[lesto::test]
async fn each_check_refuses_its_token() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = app(oidc(&provider).await);
    let sign = |extra| key.sign(&claims(&provider, extra));

    refused(
        &router,
        &sign(json!({"exp": now() - 120})),
        "The token has expired",
    )
    .await;
    refused(
        &router,
        &sign(json!({"nbf": now() + 120})),
        "The token is not valid yet",
    )
    .await;
    refused(
        &router,
        &sign(json!({"iss": "https://evil.example.com"})),
        "The token was issued by another issuer",
    )
    .await;
    refused(
        &router,
        &sign(json!({"aud": "api://other"})),
        "The token is meant for another audience",
    )
    .await;
    refused(
        &router,
        &sign(json!({"aud": null})),
        "The token is malformed",
    )
    .await;
    refused(&router, "not.a.jwt", "The token is malformed").await;
    refused(
        &router,
        &sign(json!({"exp": null})),
        "The token is malformed",
    )
    .await;
}

#[lesto::test]
async fn the_leeway_tolerates_clock_skew() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = app(oidc(&provider).await);
    // Expired ten seconds ago: within the default 30 s.
    let token = key.sign(&claims(&provider, json!({"exp": now() - 10})));
    assert_eq!(call(&router, "/me", Some(&token)).await.0, StatusCode::OK);
}

#[lesto::test]
async fn the_client_is_read_from_azp_then_client_id_then_appid() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = app(oidc(&provider).await);
    let sign = |extra| key.sign(&claims(&provider, extra));
    let ok = |token: String| {
        let router = router.clone();
        async move { call(&router, "/me", Some(&token)).await.0 }
    };

    for claim in ["azp", "client_id", "appid"] {
        let mut extra = json!({"azp": null});
        extra[claim] = json!("cli");
        assert_eq!(ok(sign(extra.clone())).await, StatusCode::OK, "{claim}");
        extra[claim] = json!("mallory");
        refused(
            &router,
            &sign(extra),
            "The token was issued to a client that is not accepted",
        )
        .await;
    }
    // The first present wins: `azp` names an unknown client, `client_id` a known one.
    refused(
        &router,
        &sign(json!({"azp": "mallory", "client_id": "web"})),
        "The token was issued to a client that is not accepted",
    )
    .await;
    // No client claim at all.
    refused(
        &router,
        &sign(json!({"azp": null})),
        "The token was issued to a client that is not accepted",
    )
    .await;
}

#[lesto::test]
async fn forged_tokens_are_refused() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = app(oidc(&provider).await);
    let valid = key.sign(&claims(&provider, json!({})));
    let payload = b64(claims(&provider, json!({"sub": "admin"}))
        .to_string()
        .as_bytes());

    // `alg: none`, no signature.
    let none = format!(
        "{}.{payload}.",
        b64(br#"{"alg":"none","typ":"JWT","kid":"k1"}"#)
    );
    let (status, _, _) = call(&router, "/me", Some(&none)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // HS256 keyed with the public key: the classic key confusion.
    let jwk = serde_json::to_value(&key.jwk).unwrap();
    let secret = jwk["n"].as_str().unwrap().as_bytes().to_vec();
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("k1".into());
    let confused = jsonwebtoken::encode(
        &header,
        &claims(&provider, json!({})),
        &EncodingKey::from_secret(&secret),
    )
    .unwrap();
    refused(
        &router,
        &confused,
        "The token is signed with an algorithm that is not accepted",
    )
    .await;

    // A valid signature over another payload.
    let mut parts: Vec<&str> = valid.split('.').collect();
    parts[1] = &payload;
    refused(&router, &parts.join("."), "The token signature is invalid").await;

    // Signed by a key the provider never published, under a `kid` it did.
    let stranger = Key::rsa("k1", RSA_2);
    refused(
        &router,
        &stranger.sign(&claims(&provider, json!({}))),
        "The token signature is invalid",
    )
    .await;
}

#[lesto::test]
async fn claims_that_do_not_fit_answer_401() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = app(oidc(&provider).await);
    // `Email` needs an `email` claim.
    let token = key.sign(&claims(&provider, json!({})));
    let (status, _, body) = call(&router, "/email", Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["detail"],
        "The token lacks claims this operation needs"
    );
}

#[lesto::test]
async fn without_audiences_and_clients_neither_is_checked() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let oidc = Oidc::discover(provider.discovery_url()).await.unwrap();
    let router = app(oidc);
    let token = key.sign(&claims(
        &provider,
        json!({"aud": "anything", "azp": "anyone"}),
    ));
    assert_eq!(call(&router, "/me", Some(&token)).await.0, StatusCode::OK);
}

#[lesto::test]
async fn a_jwt_without_a_verifier_is_a_server_error() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = App::new().routes(routes![me]).into_router();
    let token = key.sign(&claims(&provider, json!({})));
    let (status, _, body) = call(&router, "/me", Some(&token)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["detail"], "Internal Server Error");
}

// ---- keys -----------------------------------------------------------------------------------

#[lesto::test]
async fn a_rotated_key_is_fetched_once() {
    let old = Key::rsa("k1", RSA_1);
    let new = Key::rsa("k2", RSA_2);
    let provider = Provider::start(&[&old]).await;
    let oidc = Oidc::discover(provider.discovery_url())
        .min_refresh(Duration::ZERO)
        .await
        .unwrap();
    let router = app(oidc);
    assert_eq!(provider.fetches(), 1);

    provider.publish(&[&old, &new]);
    let token = new.sign(&claims(&provider, json!({})));
    assert_eq!(call(&router, "/me", Some(&token)).await.0, StatusCode::OK);
    assert_eq!(provider.fetches(), 2);
    // Known now: no further fetch.
    assert_eq!(call(&router, "/me", Some(&token)).await.0, StatusCode::OK);
    assert_eq!(provider.fetches(), 2);
}

#[lesto::test]
async fn unknown_kids_trigger_one_fetch_per_min_refresh() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let oidc = Oidc::discover(provider.discovery_url())
        .min_refresh(Duration::from_secs(3600))
        .await
        .unwrap();
    let router = app(oidc);

    // The first fetch happened at startup, less than `min_refresh` ago: none of these fetches.
    let forged = Key::rsa("forged", RSA_2);
    let requests = (0..20).map(|i| {
        let token = forged.sign(&claims(&provider, json!({"jti": i})));
        let router = router.clone();
        async move { call(&router, "/me", Some(&token)).await }
    });
    for (status, _, body) in futures_join_all(requests).await {
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["detail"], "The token is signed with an unknown key");
    }
    assert_eq!(provider.fetches(), 1);
}

#[lesto::test]
async fn unknown_kids_share_one_fetch_once_min_refresh_has_passed() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let oidc = Oidc::discover(provider.discovery_url())
        .min_refresh(Duration::from_millis(50))
        .await
        .unwrap();
    let router = app(oidc);
    lesto::tokio::time::sleep(Duration::from_millis(60)).await;

    let forged = Key::rsa("forged", RSA_2);
    let requests = (0..20).map(|i| {
        let token = forged.sign(&claims(&provider, json!({"jti": i})));
        let router = router.clone();
        async move { call(&router, "/me", Some(&token)).await.0 }
    });
    for status in futures_join_all(requests).await {
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(provider.fetches(), 2);
}

#[lesto::test]
async fn a_failed_refetch_keeps_the_known_keys() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let oidc = Oidc::discover(provider.discovery_url())
        .min_refresh(Duration::ZERO)
        .await
        .unwrap();
    let router = app(oidc);

    *provider.jwks_status.lock().unwrap() = Some(StatusCode::INTERNAL_SERVER_ERROR);
    let forged = Key::rsa("k2", RSA_2);
    let (status, _, _) = call(
        &router,
        "/me",
        Some(&forged.sign(&claims(&provider, json!({})))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(provider.fetches(), 2);

    let token = key.sign(&claims(&provider, json!({})));
    assert_eq!(call(&router, "/me", Some(&token)).await.0, StatusCode::OK);
}

/// `join_all` without a dependency: spawn each future, await the handles in order.
async fn futures_join_all<F>(futures: impl Iterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futures.map(lesto::tokio::spawn).collect();
    let mut out = Vec::new();
    for handle in handles {
        out.push(handle.await.unwrap());
    }
    out
}

// ---- discovery ------------------------------------------------------------------------------

#[lesto::test]
async fn discovery_fails_loudly() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;

    let error = Oidc::discover("http://login.example.com/.well-known/openid-configuration")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Url { .. }), "{error}");

    let error = Oidc::discover(format!("{}/missing", provider.base))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Status { .. }), "{error}");

    *provider.issuer.lock().unwrap() = "https://evil.example.com".into();
    let error = Oidc::discover(provider.discovery_url()).await.unwrap_err();
    assert!(matches!(error, Error::IssuerMismatch { .. }), "{error}");
    *provider.issuer.lock().unwrap() = provider.base.clone();

    *provider.algorithms.lock().unwrap() = json!(["HS256", "none"]);
    let error = Oidc::discover(provider.discovery_url()).await.unwrap_err();
    assert!(matches!(error, Error::NoAlgorithm { .. }), "{error}");
    *provider.algorithms.lock().unwrap() = json!(["RS256"]);

    *provider.jwks.lock().unwrap() = json!({"keys": [{"kty": "oct", "k": "c2VjcmV0"}]});
    let error = Oidc::discover(provider.discovery_url()).await.unwrap_err();
    assert!(matches!(error, Error::NoKeys { .. }), "{error}");

    let error = Oidc::discover("http://127.0.0.1:1/.well-known/openid-configuration")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Fetch { .. }), "{error}");
}

#[lesto::test]
async fn encryption_keys_and_unknown_key_types_are_skipped() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let mut set = serde_json::to_value(JwkSet {
        keys: vec![key.jwk.clone()],
    })
    .unwrap();
    let mut encryption = serde_json::to_value(&key.jwk).unwrap();
    encryption["kid"] = json!("enc");
    encryption["use"] = json!("enc");
    encryption["alg"] = json!("RSA-OAEP");
    set["keys"]
        .as_array_mut()
        .unwrap()
        .extend([encryption, json!({"kty": "future", "kid": "x"})]);
    *provider.jwks.lock().unwrap() = set;

    let router = app(oidc(&provider).await);
    let token = key.sign(&claims(&provider, json!({})));
    assert_eq!(call(&router, "/me", Some(&token)).await.0, StatusCode::OK);
}

// ---- documentation --------------------------------------------------------------------------

#[lesto::test]
async fn the_scheme_and_the_requirement_are_documented() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let oidc = oidc(&provider).await;
    let spec: Value = serde_json::from_str(
        &App::<()>::new()
            .oidc(oidc)
            .routes(routes![me, health])
            .openapi_json(),
    )
    .unwrap();
    assert_eq!(
        spec["components"]["securitySchemes"]["openIdConnect"],
        json!({"type": "openIdConnect", "openIdConnectUrl": provider.discovery_url()})
    );
    assert_eq!(
        spec["paths"]["/me"]["get"]["security"],
        json!([{"openIdConnect": []}])
    );
    assert!(spec["paths"]["/me"]["get"]["responses"]["401"].is_object());
    assert!(spec["paths"]["/health"]["get"]["security"].is_null());
}

// ---- protect --------------------------------------------------------------------------------

/// `/health` public, `/admin/*` protected and requiring the `admin` scope.
async fn protected_app(provider: &Provider) -> App {
    let admin = App::new()
        .routes(routes![stats, whoami])
        .protect(oidc(provider).await.scopes(["admin"]));
    App::new().routes(routes![health]).nest("/admin", admin)
}

#[lesto::test]
async fn protect_guards_a_nested_app_and_nothing_else() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let router = protected_app(&provider).await.into_router();

    assert_eq!(call(&router, "/health", None).await.0, StatusCode::OK);
    assert_eq!(call(&router, "/docs", None).await.0, StatusCode::OK);

    let (status, challenge, _) = call(&router, "/admin/stats", None).await;
    assert_eq!(
        (status, challenge.as_str()),
        (StatusCode::UNAUTHORIZED, "Bearer")
    );

    let reader = key.sign(&claims(&provider, json!({})));
    let (status, challenge, body) = call(&router, "/admin/stats", Some(&reader)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        challenge,
        "Bearer error=\"insufficient_scope\", scope=\"admin\""
    );

    let admin = key.sign(&claims(&provider, json!({"scp": ["admin"]})));
    let (status, _, body) = call(&router, "/admin/stats", Some(&admin)).await;
    assert_eq!((status, body), (StatusCode::OK, json!("stats")));

    // The handler's own `Jwt` reads what the layer verified: no `App::oidc` on this app.
    let (status, _, body) = call(&router, "/admin/whoami", Some(&admin)).await;
    assert_eq!((status, body), (StatusCode::OK, json!("user-1")));

    let expired = key.sign(&claims(
        &provider,
        json!({"exp": now() - 120, "scp": ["admin"]}),
    ));
    let (status, _, body) = call(&router, "/admin/stats", Some(&expired)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["detail"], "The token has expired");
}

#[lesto::test]
async fn protect_documents_only_the_protected_operations() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let app = protected_app(&provider).await;
    let spec: Value = serde_json::from_str(&app.openapi_json()).unwrap();

    assert!(spec["paths"]["/health"]["get"]["security"].is_null());
    let stats = &spec["paths"]["/admin/stats"]["get"];
    assert_eq!(stats["security"], json!([{"openIdConnect": ["admin"]}]));
    assert!(stats["responses"]["401"].is_object());
    assert!(stats["responses"]["403"].is_object());
    assert_eq!(
        spec["components"]["securitySchemes"]["openIdConnect"]["type"],
        "openIdConnect"
    );
}

#[lesto::test]
async fn protect_on_the_served_app_leaves_the_docs_public() {
    let key = Key::rsa("k1", RSA_1);
    let provider = Provider::start(&[&key]).await;
    let app = App::new()
        .routes(routes![health])
        .protect(oidc(&provider).await);
    let spec: Value = serde_json::from_str(&app.openapi_json()).unwrap();
    assert_eq!(
        spec["paths"]["/health"]["get"]["security"],
        json!([{"openIdConnect": []}])
    );
    let router = app.into_router();
    assert_eq!(
        call(&router, "/health", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(call(&router, "/openapi.json", None).await.0, StatusCode::OK);
    let token = key.sign(&claims(&provider, json!({})));
    assert_eq!(
        call(&router, "/health", Some(&token)).await.0,
        StatusCode::OK
    );
}

// ---- lesto::db ------------------------------------------------------------------------------

mod db {
    use lesto::axum::extract::FromRef;
    use lesto::db::Error;
    use lesto::db::prelude::*;
    use sqlx::Sqlite;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    #[derive(Clone)]
    pub struct AppState {
        db: Db<Sqlite>,
    }

    impl FromRef<AppState> for Db<Sqlite> {
        fn from_ref(s: &AppState) -> Self {
            s.db.clone()
        }
    }

    // Chapter 13, "A verified JWT as the credential": keep the two in step.
    #[lesto::model]
    pub struct TokenClaims {
        sub: String,
        #[serde(default)]
        scope: String,
    }

    pub struct Caller {
        pub id: String,
        scopes: Vec<String>,
    }

    impl Authenticated for Caller {
        type State = AppState;
        type Credential = Jwt<TokenClaims>;

        async fn authenticate(token: Jwt<TokenClaims>, _: &AppState) -> Result<Self, HttpError> {
            let claims = token.into_claims();
            Ok(Caller {
                id: claims.sub,
                scopes: claims.scope.split_whitespace().map(String::from).collect(),
            })
        }

        fn has_permission(&self, permission: &str) -> bool {
            self.scopes.iter().any(|s| s == permission)
        }
    }

    #[derive(lesto::db::Store)]
    struct NoteStore<M, P>(Store<M, P, Sqlite>);

    impl<M: Writable> NoteStore<M, Caller> {
        async fn create(&self, text: String) -> Result<i64, Error> {
            let author = self.principal().id.clone();
            self.write("notes:write", async |conn| {
                sqlx::query_scalar("INSERT INTO notes (author, text) VALUES (?, ?) RETURNING id")
                    .bind(&author)
                    .bind(&text)
                    .fetch_one(conn)
                    .await
            })
            .await
        }
    }

    #[lesto::post("/notes", status = 201)]
    async fn create(store: NoteStore<ReadWrite, Caller>) -> Result<Json<i64>, Error> {
        Ok(Json(store.create("hello".into()).await?))
    }

    #[lesto::test]
    async fn a_jwt_is_a_principal_credential() {
        let key = Key::rsa("k1", RSA_1);
        let provider = Provider::start(&[&key]).await;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE notes (id INTEGER PRIMARY KEY, author TEXT, text TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        let app = App::new()
            .oidc(oidc(&provider).await)
            .routes(routes![create])
            .with_state(AppState { db: Db::new(pool) });
        let spec: Value = serde_json::from_str(&app.openapi_json()).unwrap();
        assert_eq!(
            spec["paths"]["/notes"]["post"]["security"],
            json!([{"openIdConnect": []}])
        );
        let router = app.into_router();

        let post = |token: Option<String>| {
            let router = router.clone();
            async move {
                let mut request = Request::post("/notes");
                if let Some(token) = token {
                    request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
                }
                router
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap()
                    .status()
            }
        };
        assert_eq!(post(None).await, StatusCode::UNAUTHORIZED);
        let reader = key.sign(&claims(&provider, json!({})));
        assert_eq!(post(Some(reader)).await, StatusCode::FORBIDDEN);
        let writer = key.sign(&claims(
            &provider,
            json!({"scope": "notes:read notes:write"}),
        ));
        assert_eq!(post(Some(writer)).await, StatusCode::CREATED);
    }
}

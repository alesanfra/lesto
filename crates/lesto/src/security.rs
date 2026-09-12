//! Authentication extractors that document themselves as OpenAPI security schemes.
//!
//! Like FastAPI's `OAuth2PasswordBearer` / `APIKeyHeader` / `HTTPBasic`, each extractor pulls
//! credentials out of the request **and** registers a security scheme in
//! `components.securitySchemes` plus a `security` requirement on the operation. Swagger UI then
//! shows the *Authorize* button.
//!
//! ```ignore
//! /// Who am I?
//! #[lesto::get("/me")]
//! async fn me(auth: Bearer) -> Json<User> { lookup(auth.token()) }
//!
//! struct AdminKey;
//! impl ApiKeyScheme for AdminKey {
//!     const NAME: &'static str = "adminKey";
//!     const KEY: &'static str = "X-Admin-Key";
//! }
//! #[lesto::delete("/users/{id}", status = 204)]
//! async fn delete_user(key: ApiKey<AdminKey>, Path(id): Path<u64>) { .. }
//! ```
//!
//! Extractors only *extract*: verifying the token is the handler's (or a middleware's) job.

use std::marker::PhantomData;

use axum::extract::FromRequestParts;
use base64::Engine;
use http::StatusCode;
use http::header::{self, HeaderValue};
use http::request::Parts;

use crate::error::{HttpError, Rejection};
use crate::openapi::{ApiKeyIn, SecurityScheme};
use crate::operation::{OperationBuilder, OperationInput};

/// A named OpenAPI security scheme, implemented on a marker type.
///
/// Built-ins: [`BearerAuth`], [`BasicAuth`]. Implement it yourself for OAuth2 / OpenID Connect:
///
/// ```ignore
/// struct OAuth2;
/// impl AuthScheme for OAuth2 {
///     const NAME: &'static str = "oauth2";
///     fn scheme() -> SecurityScheme {
///         SecurityScheme::oauth2(OAuthFlows::password("/token").scope("read", "Read access"))
///     }
///     fn scopes() -> &'static [&'static str] { &["read"] }
/// }
/// async fn handler(auth: Bearer<OAuth2>) { .. }
/// ```
pub trait AuthScheme: Send + Sync + 'static {
    /// Key under `components.securitySchemes`.
    const NAME: &'static str;
    fn scheme() -> SecurityScheme;
    /// Scopes required by operations using this marker (OAuth2 / OpenID Connect only).
    fn scopes() -> &'static [&'static str] {
        &[]
    }
}

/// `Authorization: Bearer <token>`, documented as `http` / `bearer`.
pub struct BearerAuth;

impl AuthScheme for BearerAuth {
    const NAME: &'static str = "bearerAuth";
    fn scheme() -> SecurityScheme {
        SecurityScheme::bearer()
    }
}

/// `Authorization: Basic <base64>`, documented as `http` / `basic`.
pub struct BasicAuth;

impl AuthScheme for BasicAuth {
    const NAME: &'static str = "basicAuth";
    fn scheme() -> SecurityScheme {
        SecurityScheme::basic()
    }
}

/// An API key scheme: where the key lives and what it is called.
pub trait ApiKeyScheme: Send + Sync + 'static {
    /// Key under `components.securitySchemes`.
    const NAME: &'static str;
    /// Header, query parameter or cookie name.
    const KEY: &'static str;
    const LOCATION: ApiKeyIn = ApiKeyIn::Header;
}

fn unauthorized(detail: &str, challenge: &'static str) -> Rejection {
    HttpError::new(StatusCode::UNAUTHORIZED, detail)
        .with_header(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(challenge),
        )
        .into()
}

fn authorization<'a>(parts: &'a Parts, scheme: &str) -> Option<&'a str> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (found, rest) = value.split_once(' ')?;
    found.eq_ignore_ascii_case(scheme).then(|| rest.trim())
}

// ---- Bearer ---------------------------------------------------------------------------------

/// The token from `Authorization: Bearer <token>`. Missing or malformed → `401`.
///
/// `Debug` prints the scheme name only: the token never reaches a log through `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct Bearer<S: AuthScheme = BearerAuth> {
    token: String,
    _scheme: PhantomData<fn() -> S>,
}

impl<S: AuthScheme> std::fmt::Debug for Bearer<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bearer")
            .field("scheme", &S::NAME)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl<S: AuthScheme> Bearer<S> {
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn into_token(self) -> String {
        self.token
    }
}

impl<S: AuthScheme> std::ops::Deref for Bearer<S> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.token
    }
}

impl<S: AuthScheme, St: Send + Sync> FromRequestParts<St> for Bearer<S> {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, _state: &St) -> Result<Self, Self::Rejection> {
        match authorization(parts, "Bearer") {
            Some(token) if !token.is_empty() => Ok(Bearer {
                token: token.to_string(),
                _scheme: PhantomData,
            }),
            _ => Err(unauthorized("Not authenticated", "Bearer")),
        }
    }
}

impl<S: AuthScheme> OperationInput for Bearer<S> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.security(S::NAME, S::scheme(), S::scopes());
    }
}

// ---- Basic ----------------------------------------------------------------------------------

/// Credentials from `Authorization: Basic <base64(user:password)>`. Missing or malformed → `401`.
///
/// `Debug` prints the username and redacts the password.
#[derive(Clone, PartialEq, Eq)]
pub struct Basic<S: AuthScheme = BasicAuth> {
    pub username: String,
    pub password: String,
    _scheme: PhantomData<fn() -> S>,
}

impl<S: AuthScheme> std::fmt::Debug for Basic<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Basic")
            .field("scheme", &S::NAME)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}

impl<S: AuthScheme, St: Send + Sync> FromRequestParts<St> for Basic<S> {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, _state: &St) -> Result<Self, Self::Rejection> {
        let encoded = authorization(parts, "Basic")
            .ok_or_else(|| unauthorized("Not authenticated", "Basic"))?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(|| unauthorized("Invalid authentication credentials", "Basic"))?;
        let (username, password) = decoded
            .split_once(':')
            .ok_or_else(|| unauthorized("Invalid authentication credentials", "Basic"))?;
        Ok(Basic {
            username: username.to_string(),
            password: password.to_string(),
            _scheme: PhantomData,
        })
    }
}

impl<S: AuthScheme> OperationInput for Basic<S> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.security(S::NAME, S::scheme(), S::scopes());
    }
}

// ---- ApiKey ---------------------------------------------------------------------------------

/// An API key read from the header, query parameter or cookie named by `S`. Missing → `401`.
///
/// `Debug` prints the scheme name only: the key never reaches a log through `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey<S: ApiKeyScheme> {
    key: String,
    _scheme: PhantomData<fn() -> S>,
}

impl<S: ApiKeyScheme> std::fmt::Debug for ApiKey<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey")
            .field("scheme", &S::NAME)
            .field("key", &"[redacted]")
            .finish()
    }
}

impl<S: ApiKeyScheme> ApiKey<S> {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn into_key(self) -> String {
        self.key
    }
}

impl<S: ApiKeyScheme> std::ops::Deref for ApiKey<S> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.key
    }
}

fn cookie<'a>(parts: &'a Parts, name: &str) -> Option<&'a str> {
    parts
        .headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn query_param(parts: &Parts, name: &str) -> Option<String> {
    let query = parts.uri.query()?;
    form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

impl<S: ApiKeyScheme, St: Send + Sync> FromRequestParts<St> for ApiKey<S> {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, _state: &St) -> Result<Self, Self::Rejection> {
        let key = match S::LOCATION {
            ApiKeyIn::Header => parts
                .headers
                .get(S::KEY)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            ApiKeyIn::Query => query_param(parts, S::KEY),
            ApiKeyIn::Cookie => cookie(parts, S::KEY).map(str::to_string),
        };
        match key {
            Some(key) if !key.is_empty() => Ok(ApiKey {
                key,
                _scheme: PhantomData,
            }),
            _ => Err(HttpError::new(StatusCode::UNAUTHORIZED, "Not authenticated").into()),
        }
    }
}

impl<S: ApiKeyScheme> OperationInput for ApiKey<S> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.security(S::NAME, SecurityScheme::api_key(S::LOCATION, S::KEY), &[]);
    }
}

// ---- Security marker ------------------------------------------------------------------------

/// Documents that the operation requires scheme `S` without extracting anything.
///
/// For routes protected by a middleware (e.g. `tower_http::validate_request`) that should still
/// show a lock in the docs.
pub struct Security<S: AuthScheme>(PhantomData<fn() -> S>);

impl<S: AuthScheme, St: Send + Sync> FromRequestParts<St> for Security<S> {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(_parts: &mut Parts, _state: &St) -> Result<Self, Self::Rejection> {
        Ok(Security(PhantomData))
    }
}

impl<S: AuthScheme> OperationInput for Security<S> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.security(S::NAME, S::scheme(), S::scopes());
    }
}

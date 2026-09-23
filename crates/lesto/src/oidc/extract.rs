//! The `Jwt<C>` extractor.

use std::sync::Arc;

use axum::extract::FromRequestParts;
use http::request::Parts;
use serde::de::IntoDeserializer;
use serde_json::{Map, Value};

use super::claims::{Claims, StandardClaims, scopes};
use super::verify::TokenError;
use super::{Oidc, SCHEME_NAME};
use crate::error::{HttpError, Rejection};
use crate::operation::{OperationBuilder, OperationInput};

/// The claims of a verified token, in the request extensions: the first extractor (or
/// [`App::protect`](crate::App::protect)) verifies, the others read.
#[derive(Clone)]
pub(super) struct Verified(pub(super) Arc<Map<String, Value>>);

/// A bearer token verified by the [`Oidc`] verifier of the app, and its claims read into `C`.
///
/// ```ignore
/// #[lesto::model]
/// struct Claims { sub: String, email: String }
///
/// #[lesto::get("/me")]
/// async fn me(token: Jwt<Claims>) -> String { token.claims().email.clone() }
/// ```
///
/// The verifier comes from [`App::oidc`](crate::App::oidc) (or
/// [`App::protect`](crate::App::protect)); a `Jwt` on an app with neither answers `500` and
/// logs why. A missing token answers `401` with a bare `Bearer` challenge, an invalid one `401`
/// with `error="invalid_token"` (see [`TokenError`]). The operation is documented with the
/// `openIdConnect` security scheme.
///
/// The raw claims stay available ([`claim`](Self::claim), [`scopes`](Self::scopes)), whatever
/// `C` keeps of them. The token itself is not kept.
pub struct Jwt<C: Claims = StandardClaims> {
    claims: C,
    raw: Arc<Map<String, Value>>,
}

impl<C: Claims + std::fmt::Debug> std::fmt::Debug for Jwt<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jwt").field("claims", &self.claims).finish()
    }
}

impl<C: Claims + Clone> Clone for Jwt<C> {
    fn clone(&self) -> Self {
        Jwt {
            claims: self.claims.clone(),
            raw: self.raw.clone(),
        }
    }
}

impl<C: Claims> Jwt<C> {
    /// The claims, as `C`.
    pub fn claims(&self) -> &C {
        &self.claims
    }

    /// The claims, owned.
    pub fn into_claims(self) -> C {
        self.claims
    }

    /// `sub`: who the token is about.
    pub fn subject(&self) -> Option<&str> {
        self.raw.get("sub").and_then(Value::as_str)
    }

    /// The scopes granted, from `scope` (space separated) or `scp` (a list or a string).
    pub fn scopes(&self) -> impl Iterator<Item = &str> {
        scopes(&self.raw)
    }

    /// Does the token grant `scope`?
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes().any(|s| s == scope)
    }

    /// Any claim, as JSON.
    pub fn claim(&self, name: &str) -> Option<&Value> {
        self.raw.get(name)
    }

    /// Every claim, as JSON, in the order the token lists them.
    pub fn raw_claims(&self) -> &Map<String, Value> {
        &self.raw
    }
}

impl<C: Claims> std::ops::Deref for Jwt<C> {
    type Target = C;
    fn deref(&self) -> &C {
        &self.claims
    }
}

/// The token of `Authorization: Bearer <token>`.
pub(super) fn bearer(parts: &Parts) -> Option<&str> {
    let value = parts
        .headers
        .get(http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty()).then_some(token)
}

/// Verify the request's token once: later calls read the claims the first one stored.
pub(super) async fn verified(
    parts: &mut Parts,
    oidc: Option<&Oidc>,
) -> Result<Arc<Map<String, Value>>, Rejection> {
    if let Some(Verified(claims)) = parts.extensions.get::<Verified>() {
        return Ok(claims.clone());
    }
    let Some(oidc) = oidc.or_else(|| parts.extensions.get::<Oidc>()).cloned() else {
        tracing::error!(
            "a `Jwt` argument needs a verifier: call `App::oidc(..)` or `App::protect(..)` \
             on the app serving this route"
        );
        return Err(HttpError::internal("Internal Server Error").into());
    };
    let token = bearer(parts).ok_or_else(|| TokenError::Missing.into_http_error())?;
    let claims = Arc::new(
        oidc.verify(token)
            .await
            .map_err(TokenError::into_http_error)?,
    );
    parts.extensions.insert(Verified(claims.clone()));
    Ok(claims)
}

impl<C: Claims, S: Send + Sync> FromRequestParts<S> for Jwt<C> {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let raw = verified(parts, None).await?;
        let claims = C::deserialize(raw.as_ref().into_deserializer()).map_err(|e| {
            tracing::debug!(error = %e, "the token's claims do not fit {}", std::any::type_name::<C>());
            TokenError::Claims.into_http_error()
        })?;
        Ok(Jwt { claims, raw })
    }
}

impl<C: Claims> OperationInput for Jwt<C> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.security_requirement(SCHEME_NAME, &[]);
    }
}

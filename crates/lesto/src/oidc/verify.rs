//! The checks a token goes through, and what a failure answers.

use http::StatusCode;
use http::header::{self, HeaderValue};
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Validation, decode, decode_header};
use serde_json::{Map, Value};

use super::Inner;
use crate::error::HttpError;

/// Why a token was refused. Each variant is a class, not the detail: the detail goes to
/// `tracing` at `debug`, and the response names only the class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenError {
    /// No `Authorization: Bearer` header.
    Missing,
    /// Not a JWT: wrong number of segments, bad base64, bad JSON, a required claim missing.
    Malformed,
    /// Signed with an algorithm the provider does not announce, or a symmetric one, or `none`.
    Algorithm,
    /// The header's `kid` names no key of the provider, even after fetching the keys again.
    UnknownKey,
    /// The signature does not verify.
    Signature,
    /// `exp` is in the past, beyond the leeway.
    Expired,
    /// `nbf` is in the future, beyond the leeway.
    NotYetValid,
    /// `iss` is not the provider's issuer.
    Issuer,
    /// `aud` contains none of the accepted audiences.
    Audience,
    /// The client (`azp`, `client_id` or `appid`) is not one of the accepted clients.
    Client,
    /// The claims do not deserialize into the type the handler asked for.
    Claims,
}

impl TokenError {
    /// The text of the problem's `detail` and of `error_description`.
    pub fn description(self) -> &'static str {
        match self {
            TokenError::Missing => "Not authenticated",
            TokenError::Malformed => "The token is malformed",
            TokenError::Algorithm => "The token is signed with an algorithm that is not accepted",
            TokenError::UnknownKey => "The token is signed with an unknown key",
            TokenError::Signature => "The token signature is invalid",
            TokenError::Expired => "The token has expired",
            TokenError::NotYetValid => "The token is not valid yet",
            TokenError::Issuer => "The token was issued by another issuer",
            TokenError::Audience => "The token is meant for another audience",
            TokenError::Client => "The token was issued to a client that is not accepted",
            TokenError::Claims => "The token lacks claims this operation needs",
        }
    }

    /// The `401` problem, with the `WWW-Authenticate` challenge of RFC 6750: a bare `Bearer`
    /// when no token was sent, `error="invalid_token"` otherwise.
    pub fn into_http_error(self) -> HttpError {
        let challenge = match self {
            TokenError::Missing => HeaderValue::from_static("Bearer"),
            other => HeaderValue::from_str(&format!(
                "Bearer error=\"invalid_token\", error_description=\"{}\"",
                other.description()
            ))
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer error=\"invalid_token\"")),
        };
        HttpError::new(StatusCode::UNAUTHORIZED, self.description())
            .with_header(header::WWW_AUTHENTICATE, challenge)
    }
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}

impl std::error::Error for TokenError {}

impl axum::response::IntoResponse for TokenError {
    fn into_response(self) -> axum::response::Response {
        self.into_http_error().into_response()
    }
}

/// Log the reason at `debug` and hand the class back.
fn refuse(class: TokenError, reason: &dyn std::fmt::Display) -> TokenError {
    tracing::debug!(%reason, "bearer token refused: {}", class.description());
    class
}

fn classify(kind: &ErrorKind) -> TokenError {
    match kind {
        ErrorKind::InvalidSignature => TokenError::Signature,
        ErrorKind::ExpiredSignature => TokenError::Expired,
        ErrorKind::ImmatureSignature => TokenError::NotYetValid,
        ErrorKind::InvalidIssuer => TokenError::Issuer,
        ErrorKind::InvalidAudience => TokenError::Audience,
        ErrorKind::InvalidAlgorithm
        | ErrorKind::MissingAlgorithm
        | ErrorKind::InvalidAlgorithmName
        | ErrorKind::UnsupportedAlgorithm => TokenError::Algorithm,
        // A key that does not fit the algorithm: an RSA key named by an ES256 token.
        ErrorKind::InvalidKeyFormat | ErrorKind::InvalidEcdsaKey | ErrorKind::InvalidRsaKey(_) => {
            TokenError::Algorithm
        }
        _ => TokenError::Malformed,
    }
}

/// Every check of the module documentation, in its order.
pub(super) async fn verify(inner: &Inner, token: &str) -> Result<Map<String, Value>, TokenError> {
    let header = decode_header(token).map_err(|e| refuse(TokenError::Malformed, &e))?;
    if !inner.algorithms.contains(&header.alg) {
        return Err(refuse(
            TokenError::Algorithm,
            &format_args!("{:?} is not among {:?}", header.alg, inner.algorithms),
        ));
    }
    let key = inner
        .keys
        .find(header.kid.as_deref())
        .await
        .ok_or_else(|| {
            refuse(
                TokenError::UnknownKey,
                &format_args!("kid {:?}", header.kid),
            )
        })?;
    if key.algorithm.is_some_and(|alg| alg != header.alg) {
        return Err(refuse(
            TokenError::Algorithm,
            &format_args!(
                "the key is for {:?}, the token says {:?}",
                key.algorithm, header.alg
            ),
        ));
    }

    let mut validation = Validation::new(header.alg);
    validation.leeway = inner.leeway.as_secs();
    validation.validate_nbf = true;
    validation.set_issuer(&[&inner.issuer]);
    if inner.audiences.is_empty() {
        validation.validate_aud = false;
        validation.set_required_spec_claims(&["exp", "iss"]);
    } else {
        validation.set_audience(&inner.audiences);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
    }
    let claims = decode::<Map<String, Value>>(token, &key.decoding, &validation)
        .map_err(|e| refuse(classify(e.kind()), &e))?
        .claims;

    if !inner.clients.is_empty() {
        let client = ["azp", "client_id", "appid"]
            .into_iter()
            .find_map(|name| claims.get(name))
            .and_then(Value::as_str);
        if !client.is_some_and(|client| inner.clients.iter().any(|c| c == client)) {
            return Err(refuse(
                TokenError::Client,
                &format_args!("client {client:?}"),
            ));
        }
    }
    Ok(claims)
}

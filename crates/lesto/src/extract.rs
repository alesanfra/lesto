//! Validating extractors: `Json`, `Query`, `Path`.
//!
//! `Json` and `Query` deserialize **and** run `garde` validation; on failure they answer with
//! an RFC 9457 `422`. `Path` deserializes only (path parameters are scalars, which garde
//! does not validate) but still reports failures as `422`.

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::response::{IntoResponse, Response};
use garde::Validate;
use http::request::Parts;
use http::{HeaderValue, StatusCode, header};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{HttpError, Rejection, ValidationError};
use crate::openapi::ParameterIn;
use crate::operation::{OperationBuilder, OperationInput, OperationOutput};

/// JSON request body (deserialized and validated) or JSON response body (serialized).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Json<T>(pub T);

impl<T> From<T> for Json<T> {
    fn from(value: T) -> Self {
        Json(value)
    }
}

impl<T> std::ops::Deref for Json<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> std::ops::DerefMut for Json<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

fn is_json_content_type(headers: &http::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<mime::Mime>().ok())
        .map(|m| {
            m.type_() == "application"
                && (m.subtype() == "json" || m.suffix().is_some_and(|s| s == "json"))
        })
        .unwrap_or(false)
}

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned + Validate,
    T::Context: Default,
    S: Send + Sync,
{
    type Rejection = Rejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json_content_type(req.headers()) {
            return Err(HttpError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Expected request with `Content-Type: application/json`",
            )
            .into());
        }
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| HttpError::new(e.status(), e.body_text()))?;
        let mut de = serde_json::Deserializer::from_slice(&bytes);
        let value: T = serde_path_to_error::deserialize(&mut de).map_err(|e| {
            let msg = e.inner().to_string();
            if e.inner().is_syntax() || e.inner().is_eof() {
                ValidationError::single(vec!["body".into()], msg, "json_invalid")
            } else {
                ValidationError::from_serde_path("body", e.path(), msg)
            }
        })?;
        value
            .validate()
            .map_err(|report| ValidationError::from_garde("body", &report))?;
        Ok(Json(value))
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                )],
                bytes,
            )
                .into_response(),
            Err(err) => {
                tracing::error!(
                    error = &err as &dyn std::error::Error,
                    "response body failed to serialize"
                );
                HttpError::internal("Internal Server Error").into_response()
            }
        }
    }
}

impl<T: JsonSchema> OperationInput for Json<T> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.request_body::<T>("application/json", true);
        builder.validation_error_response();
    }
}

impl<T: JsonSchema> OperationOutput for Json<T> {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        builder.response::<T>(
            status,
            &crate::operation::reason_phrase(status),
            "application/json",
        );
    }
}

/// Query string (deserialized and validated).
///
/// A repeated key (`?tag=a&tag=b`) deserializes into a `Vec<T>` field; a missing repeated key
/// needs `#[serde(default)]` on the field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Query<T>(pub T);

impl<T> std::ops::Deref for Query<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned + Validate,
    T::Context: Default,
    S: Send + Sync,
{
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // `serde_html_form` collects repeated keys (`?tag=a&tag=b`) into sequences, which
        // `serde_urlencoded` cannot do.
        let query = parts.uri.query().unwrap_or_default();
        let de = serde_html_form::Deserializer::new(form_urlencoded::parse(query.as_bytes()));
        let value: T = serde_path_to_error::deserialize(de).map_err(|e| {
            ValidationError::from_serde_path("query", e.path(), e.inner().to_string())
        })?;
        value
            .validate()
            .map_err(|report| ValidationError::from_garde("query", &report))?;
        Ok(Query(value))
    }
}

impl<T: JsonSchema> OperationInput for Query<T> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.parameters_from_object::<T>(ParameterIn::Query);
        builder.validation_error_response();
    }
}

/// Path parameters, reported as `422` when they fail to parse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Path<T>(pub T);

impl<T> std::ops::Deref for Path<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        use axum::extract::path::ErrorKind;
        use axum::extract::rejection::PathRejection;

        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(value)) => Ok(Path(value)),
            Err(PathRejection::FailedToDeserializePathParams(err)) => {
                // Scalar and tuple extractors do not know their parameter names; recover them
                // from the matched route template.
                let names = parts
                    .extensions
                    .get::<axum::extract::MatchedPath>()
                    .map(|m| crate::openapi::path_param_names(m.as_str()))
                    .unwrap_or_default();
                let key = match err.kind() {
                    ErrorKind::ParseErrorAtKey { key, .. }
                    | ErrorKind::InvalidUtf8InPathParam { key }
                    | ErrorKind::DeserializeError { key, .. } => Some(key.clone()),
                    ErrorKind::ParseErrorAtIndex { index, .. } => names.get(*index).cloned(),
                    ErrorKind::ParseError { .. } if names.len() == 1 => names.first().cloned(),
                    _ => None,
                };
                let mut loc: Vec<serde_json::Value> = vec!["path".into()];
                if let Some(key) = key {
                    loc.push(key.into());
                }
                Err(ValidationError::single(loc, err.kind().to_string(), "value_error").into())
            }
            Err(other) => Err(HttpError::new(other.status(), other.body_text()).into()),
        }
    }
}

impl<T: JsonSchema> OperationInput for Path<T> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.path_parameters::<T>();
        builder.validation_error_response();
    }
}

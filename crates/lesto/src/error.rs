//! Error types.
//!
//! Errors are serialized as **RFC 9457 Problem Details** (`application/problem+json`) by default.
//! [`HttpError`] is the equivalent of FastAPI's `HTTPException`; [`ValidationError`] is the
//! `422` produced by the validating extractors, with one entry per failed check in the `errors`
//! extension member. [`ErrorFormat::FastApi`] switches the wire format to FastAPI's
//! `{"detail": ...}` shape for clients that expect it.

use axum::response::{IntoResponse, Response};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Media type of RFC 9457 responses.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// Wire format used for `HttpError` and `ValidationError` responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorFormat {
    /// RFC 9457 Problem Details, `application/problem+json` (default).
    #[default]
    Problem,
    /// FastAPI's `{"detail": "..."}` / `{"detail": [{"loc", "msg", "type"}]}`.
    FastApi,
}

/// Anything that can be turned into a [`StatusCode`]: a `StatusCode` or a bare `u16`.
pub trait IntoStatus {
    fn into_status(self) -> StatusCode;
}

impl IntoStatus for StatusCode {
    fn into_status(self) -> StatusCode {
        self
    }
}

impl IntoStatus for u16 {
    fn into_status(self) -> StatusCode {
        StatusCode::from_u16(self).expect("invalid HTTP status code")
    }
}

fn reason(status: StatusCode) -> String {
    status.canonical_reason().unwrap_or("Error").to_string()
}

// ---- RFC 9457 -----------------------------------------------------------------------------

/// An RFC 9457 problem details document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Problem {
    /// URI identifying the problem type. `about:blank` when the status code says it all.
    #[serde(rename = "type", default = "about_blank")]
    pub type_uri: String,
    /// Short, human readable summary; the HTTP reason phrase when `type` is `about:blank`.
    pub title: String,
    /// HTTP status code.
    pub status: u16,
    /// Human readable explanation specific to this occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// URI of the specific occurrence, usually the request path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Extension member: individual validation failures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ProblemError>,
    /// Any other extension members.
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

fn about_blank() -> String {
    "about:blank".to_string()
}

/// One failed check inside a [`Problem`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProblemError {
    /// Where the value came from: `body`, `query`, `path`.
    #[serde(rename = "in")]
    pub location: String,
    /// RFC 6901 JSON Pointer to the offending value within that location (`""` is the whole document).
    pub pointer: String,
    /// Human readable message.
    pub detail: String,
    /// Machine readable error class (`missing`, `value_error`, `json_invalid`, ...).
    pub code: String,
}

impl Problem {
    pub fn new(status: StatusCode) -> Self {
        Self {
            type_uri: about_blank(),
            title: reason(status),
            status: status.as_u16(),
            detail: None,
            instance: None,
            errors: Vec::new(),
            extensions: Map::new(),
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        match serde_json::to_vec(&self) {
            Ok(bytes) => (
                status,
                [(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON))],
                bytes,
            )
                .into_response(),
            Err(_) => status.into_response(),
        }
    }
}

// ---- HttpError ----------------------------------------------------------------------------

/// An HTTP error with a status and a human readable detail.
///
/// Answers as an RFC 9457 problem: `{"type": "about:blank", "title": "Not Found", "status": 404,
/// "detail": "..."}`. Use [`with_type`](Self::with_type) to give the problem a type URI and
/// [`with_extension`](Self::with_extension) to add extension members.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpError {
    pub status: StatusCode,
    pub detail: String,
    /// Rarely used members, boxed to keep `Result<T, HttpError>` small.
    extras: Option<Box<HttpErrorExtras>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct HttpErrorExtras {
    type_uri: Option<String>,
    title: Option<String>,
    extensions: Map<String, Value>,
    headers: HeaderMap,
}

/// FastAPI-shaped body of an [`HttpError`] (used with [`ErrorFormat::FastApi`]).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HttpErrorBody {
    pub detail: String,
}

impl HttpError {
    pub fn new(status: impl IntoStatus, detail: impl Into<String>) -> Self {
        Self {
            status: status.into_status(),
            detail: detail.into(),
            extras: None,
        }
    }

    fn extras(&mut self) -> &mut HttpErrorExtras {
        self.extras.get_or_insert_with(Default::default)
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, detail)
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, detail)
    }

    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, detail)
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, detail)
    }

    /// Set the problem `type` URI (and optionally a `title`, which otherwise stays the reason phrase).
    pub fn with_type(mut self, type_uri: impl Into<String>) -> Self {
        self.extras().type_uri = Some(type_uri.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.extras().title = Some(title.into());
        self
    }

    /// Add a response header, e.g. `WWW-Authenticate` or `Retry-After`.
    pub fn with_header(
        mut self,
        name: header::HeaderName,
        value: impl TryInto<HeaderValue>,
    ) -> Self {
        if let Ok(value) = value.try_into() {
            self.extras().headers.insert(name, value);
        }
        self
    }

    /// Add an RFC 9457 extension member.
    pub fn with_extension(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        if let Ok(value) = serde_json::to_value(value) {
            self.extras().extensions.insert(key.into(), value);
        }
        self
    }

    pub fn type_uri(&self) -> Option<&str> {
        self.extras.as_ref().and_then(|e| e.type_uri.as_deref())
    }

    pub fn title(&self) -> Option<&str> {
        self.extras.as_ref().and_then(|e| e.title.as_deref())
    }

    pub fn headers(&self) -> Option<&HeaderMap> {
        self.extras.as_ref().map(|e| &e.headers)
    }

    pub fn extensions(&self) -> Option<&Map<String, Value>> {
        self.extras.as_ref().map(|e| &e.extensions)
    }

    /// Split into the problem document and the extra response headers.
    pub fn into_parts(self) -> (Problem, HeaderMap) {
        let mut problem = Problem::new(self.status);
        problem.detail = Some(self.detail);
        let mut headers = HeaderMap::new();
        if let Some(extras) = self.extras {
            let extras = *extras;
            if let Some(t) = extras.type_uri {
                problem.type_uri = t;
            }
            if let Some(t) = extras.title {
                problem.title = t;
            }
            problem.extensions = extras.extensions;
            headers = extras.headers;
        }
        (problem, headers)
    }

    pub fn into_problem(self) -> Problem {
        self.into_parts().0
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.detail)
    }
}

impl std::error::Error for HttpError {}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let (problem, headers) = self.into_parts();
        let mut response = problem.into_response();
        response.headers_mut().extend(headers);
        response
    }
}

// ---- ValidationError ----------------------------------------------------------------------

/// One failed check, location-first (`["body", "addresses", 1, "city"]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ValidationErrorItem {
    /// Location of the offending value, e.g. `["body", "email"]` or `["query", "page"]`.
    pub loc: Vec<Value>,
    /// Human readable message.
    pub msg: String,
    /// Machine readable error class (`"missing"`, `"value_error"`, `"json_invalid"`, ...).
    #[serde(rename = "type")]
    pub kind: String,
}

impl ValidationErrorItem {
    /// Split the location into the source (`body`, `query`, `path`) and an RFC 6901 pointer.
    pub fn location_and_pointer(&self) -> (String, String) {
        let mut parts = self.loc.iter();
        let location = parts
            .next()
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        let pointer = parts
            .map(|v| {
                let s = v
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string());
                format!("/{}", s.replace('~', "~0").replace('/', "~1"))
            })
            .collect::<String>();
        (location, pointer)
    }

    pub fn to_problem_error(&self) -> ProblemError {
        let (location, pointer) = self.location_and_pointer();
        ProblemError {
            location,
            pointer,
            detail: self.msg.clone(),
            code: self.kind.clone(),
        }
    }

    /// Inverse of [`to_problem_error`](Self::to_problem_error).
    pub fn from_problem_error(e: &ProblemError) -> Self {
        let mut loc: Vec<Value> = vec![e.location.clone().into()];
        for segment in e.pointer.split('/').skip(1) {
            let segment = segment.replace("~1", "/").replace("~0", "~");
            loc.push(match segment.parse::<u64>() {
                Ok(n) => n.into(),
                Err(_) => segment.into(),
            });
        }
        Self {
            loc,
            msg: e.detail.clone(),
            kind: e.code.clone(),
        }
    }
}

/// FastAPI-shaped body of a `422` (used with [`ErrorFormat::FastApi`]).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(rename = "HTTPValidationError")]
pub struct HttpValidationError {
    pub detail: Vec<ValidationErrorItem>,
}

/// A failed deserialization or [`garde`] validation. Always a 422.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub errors: Vec<ValidationErrorItem>,
}

impl ValidationError {
    pub fn new(errors: Vec<ValidationErrorItem>) -> Self {
        Self { errors }
    }

    /// A single error at `loc`.
    pub fn single(loc: Vec<Value>, msg: impl Into<String>, kind: impl Into<String>) -> Self {
        Self::new(vec![ValidationErrorItem {
            loc,
            msg: msg.into(),
            kind: kind.into(),
        }])
    }

    /// Convert a garde report into a list of items, prefixing every location with `root`.
    pub fn from_garde(root: &str, report: &garde::Report) -> Self {
        let errors = report
            .iter()
            .map(|(path, error)| {
                let mut loc: Vec<Value> = vec![root.into()];
                loc.extend(garde_path_components(path));
                ValidationErrorItem {
                    loc,
                    msg: error.message().to_string(),
                    kind: "value_error".to_string(),
                }
            })
            .collect();
        Self::new(errors)
    }

    /// Convert a `serde_path_to_error` path into location components.
    pub fn from_serde_path(root: &str, path: &serde_path_to_error::Path, msg: String) -> Self {
        use serde_path_to_error::Segment;
        let mut loc: Vec<Value> = vec![root.into()];
        for segment in path.iter() {
            match segment {
                Segment::Seq { index } => loc.push((*index).into()),
                Segment::Map { key } => loc.push(key.clone().into()),
                Segment::Enum { variant } => loc.push(variant.clone().into()),
                Segment::Unknown => {}
            }
        }
        // serde reports a missing field on the parent, e.g. "missing field `email`":
        // surface the field in the location like FastAPI does.
        let kind = if let Some(field) = msg
            .strip_prefix("missing field `")
            .and_then(|r| r.split('`').next())
        {
            loc.push(field.into());
            "missing"
        } else {
            "value_error"
        };
        Self::single(loc, msg, kind)
    }

    pub fn into_problem(self) -> Problem {
        let mut problem = Problem::new(StatusCode::UNPROCESSABLE_ENTITY);
        problem.detail = Some(match self.errors.len() {
            1 => "1 validation error".to_string(),
            n => format!("{n} validation errors"),
        });
        problem.errors = self
            .errors
            .iter()
            .map(ValidationErrorItem::to_problem_error)
            .collect();
        problem
    }
}

fn garde_path_components(path: &garde::Path) -> Vec<Value> {
    use garde::error::Kind;
    path.__iter()
        .rev()
        // `inner(..)` rules on `Option`/`Vec` add a keyless component: it names nothing.
        .filter(|(kind, component)| !(*kind == Kind::None && component.is_empty()))
        .map(|(kind, component)| match kind {
            Kind::Index => component
                .parse::<u64>()
                .map(Value::from)
                .unwrap_or_else(|_| component.as_str().into()),
            _ => component.as_str().into(),
        })
        .collect()
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, e) in self.errors.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            let (location, pointer) = e.location_and_pointer();
            write!(f, "{location}{pointer}: {}", e.msg)?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

impl IntoResponse for ValidationError {
    fn into_response(self) -> Response {
        self.into_problem().into_response()
    }
}

/// Rejection produced by lesto's extractors.
#[derive(Debug)]
pub enum Rejection {
    Validation(ValidationError),
    Http(HttpError),
}

impl From<ValidationError> for Rejection {
    fn from(e: ValidationError) -> Self {
        Rejection::Validation(e)
    }
}

impl From<HttpError> for Rejection {
    fn from(e: HttpError) -> Self {
        Rejection::Http(e)
    }
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        match self {
            Rejection::Validation(e) => e.into_response(),
            Rejection::Http(e) => e.into_response(),
        }
    }
}

// ---- FastAPI conversion -------------------------------------------------------------------

/// Render a problem in FastAPI's shape: `{"detail": [...]}` for validation problems,
/// `{"detail": "..."}` otherwise.
pub fn problem_to_fastapi(problem: &Problem) -> Value {
    if problem.errors.is_empty() {
        let detail = problem
            .detail
            .clone()
            .unwrap_or_else(|| problem.title.clone());
        serde_json::to_value(HttpErrorBody { detail }).expect("serializable")
    } else {
        let detail = problem
            .errors
            .iter()
            .map(ValidationErrorItem::from_problem_error)
            .collect();
        serde_json::to_value(HttpValidationError { detail }).expect("serializable")
    }
}

// ---- OpenAPI ------------------------------------------------------------------------------

/// With the `anyhow` feature: any `anyhow::Error` becomes a 500 with the detail hidden and the
/// cause logged through `tracing`.
#[cfg(feature = "anyhow")]
impl From<anyhow::Error> for HttpError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = &*e as &dyn std::error::Error, "handler failed");
        HttpError::internal("Internal Server Error")
    }
}

impl crate::operation::OperationOutput for HttpError {
    fn describe(builder: &mut crate::operation::OperationBuilder<'_>, _status: u16) {
        builder.error_response("default", "Error");
    }
}

impl crate::operation::OperationOutput for ValidationError {
    fn describe(builder: &mut crate::operation::OperationBuilder<'_>, _status: u16) {
        builder.validation_error_response();
    }
}

impl crate::operation::OperationOutput for Rejection {
    fn describe(builder: &mut crate::operation::OperationBuilder<'_>, status: u16) {
        HttpError::describe(builder, status);
        ValidationError::describe(builder, status);
    }
}

impl crate::operation::OperationOutput for Problem {
    fn describe(builder: &mut crate::operation::OperationBuilder<'_>, _status: u16) {
        builder.error_response("default", "Error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loc_round_trips_through_json_pointer() {
        let item = ValidationErrorItem {
            loc: vec!["body".into(), "a/b".into(), 1.into(), "c~d".into()],
            msg: "bad".into(),
            kind: "value_error".into(),
        };
        let pe = item.to_problem_error();
        assert_eq!(pe.location, "body");
        assert_eq!(pe.pointer, "/a~1b/1/c~0d");
        assert_eq!(ValidationErrorItem::from_problem_error(&pe), item);
    }

    #[test]
    fn whole_document_pointer_is_empty() {
        let item = ValidationErrorItem {
            loc: vec!["body".into()],
            msg: "bad json".into(),
            kind: "json_invalid".into(),
        };
        assert_eq!(item.to_problem_error().pointer, "");
    }

    #[test]
    fn http_error_stays_small() {
        assert!(
            std::mem::size_of::<HttpError>() <= 48,
            "{}",
            std::mem::size_of::<HttpError>()
        );
    }

    #[test]
    fn http_error_becomes_problem() {
        let p = HttpError::not_found("gone")
            .with_extension("id", 7)
            .into_problem();
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(
            v,
            json!({"type": "about:blank", "title": "Not Found", "status": 404, "detail": "gone", "id": 7})
        );
    }
}

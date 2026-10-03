//! Error types.
//!
//! Errors are serialized as **RFC 9457 Problem Details** (`application/problem+json`) by default.
//! [`HttpError`] is the equivalent of FastAPI's `HTTPException`; [`ValidationError`] is the
//! `422` produced by the validating extractors, with one entry per failed check in the `errors`
//! extension member. There is one wire format: a client that wants another one rewrites the
//! body in a layer of its own.

use axum::response::{IntoResponse, Response};
use http::{HeaderMap, HeaderValue, StatusCode, header};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Media type of RFC 9457 responses.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// Anything that can be turned into a [`StatusCode`]: a `StatusCode` or a bare `u16`.
///
/// A `u16` outside `100..=599` (the range RFC 9110 defines) never panics: it becomes
/// `500 Internal Server Error` and the offending value is logged at `error`. A code read from an
/// upstream response can be passed as is.
pub trait IntoStatus {
    /// The status code.
    fn into_status(self) -> StatusCode;
}

impl IntoStatus for StatusCode {
    fn into_status(self) -> StatusCode {
        self
    }
}

impl IntoStatus for u16 {
    fn into_status(self) -> StatusCode {
        match self {
            100..=599 => StatusCode::from_u16(self).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            _ => {
                tracing::error!(status = self, "invalid HTTP status code, answering 500");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
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
    /// A problem with `title` set to the reason phrase of `status` and `type` to `about:blank`.
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
        render(self)
    }
}

// ---- rendering ----------------------------------------------------------------------------

/// What a problem needs to know about the request it answers: the `instance` to fill in.
///
/// Set by [`ProblemLayer`](crate::layers::ProblemLayer) around every request it handles, read
/// by [`render`]. Outside that layer (a plain `axum::Router`, a problem built in a spawned
/// task) there is none, and a problem renders without `instance`.
///
/// The request URI is cloned once per request (reference-counted bytes); only the path is
/// used, and only when a problem is actually rendered, so the `String` is paid for on the
/// error path alone.
#[derive(Clone)]
pub(crate) struct RenderContext {
    pub(crate) uri: http::Uri,
}

tokio::task_local! {
    pub(crate) static RENDER: RenderContext;
}

/// Marker extension on a response whose body is final: `instance` is filled in.
///
/// [`ProblemLayer`](crate::layers::ProblemLayer) leaves such a response alone. A
/// `problem+json` response without it — built by hand, or built where the layer's context
/// could not be seen (a spawned task) — goes through the layer's slow path, which parses and
/// rewrites it.
#[derive(Debug, Clone, Copy)]
pub struct ProblemRendered;

/// Turn a problem into its final response, with the `instance` the current request asks for.
pub(crate) fn render(mut problem: Problem) -> Response {
    let status = StatusCode::from_u16(problem.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let rendered = RENDER
        .try_with(|context| {
            if problem.instance.is_none() {
                problem.instance = Some(context.uri.path().to_owned());
            }
        })
        .is_ok();
    let Ok(body) = serde_json::to_vec(&problem) else {
        // Only an extension member can fail to serialize, and only a custom `Serialize` can do
        // that; the status still says what happened.
        return status.into_response();
    };
    let mut response = (
        status,
        [(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON))],
        body,
    )
        .into_response();
    if rendered {
        // Without a context there is no `instance`, so the response is *not* final: a problem
        // built in a spawned task still goes through the layer's slow path and comes out like
        // every other one.
        response.extensions_mut().insert(ProblemRendered);
    }
    response
}

// ---- HttpError ----------------------------------------------------------------------------

/// An HTTP error with a status and a human readable detail.
///
/// Answers as an RFC 9457 problem: `{"type": "about:blank", "title": "Not Found", "status": 404,
/// "detail": "..."}`. Use [`with_type`](Self::with_type) to give the problem a type URI and
/// [`with_extension`](Self::with_extension) to add extension members.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpError {
    status: StatusCode,
    detail: String,
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

impl HttpError {
    /// A problem with any status. A `u16` outside `100..=599` becomes a `500` (see [`IntoStatus`]).
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

    /// `400 Bad Request`.
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, detail)
    }

    /// `401 Unauthorized`.
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, detail)
    }

    /// `403 Forbidden`.
    pub fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, detail)
    }

    /// `404 Not Found`.
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }

    /// `409 Conflict`.
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, detail)
    }

    /// `500 Internal Server Error`. The detail is sent to the client: keep causes in the log.
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, detail)
    }

    /// Set the problem `type` URI (and optionally a `title`, which otherwise stays the reason phrase).
    pub fn with_type(mut self, type_uri: impl Into<String>) -> Self {
        self.extras().type_uri = Some(type_uri.into());
        self
    }

    /// Set the `title` member (default: the reason phrase of the status).
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

    /// Status code of the response.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The `detail` member: what went wrong in this occurrence.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// The `type` member, if set.
    pub fn type_uri(&self) -> Option<&str> {
        self.extras.as_ref().and_then(|e| e.type_uri.as_deref())
    }

    /// The `title` member, if set.
    pub fn title(&self) -> Option<&str> {
        self.extras.as_ref().and_then(|e| e.title.as_deref())
    }

    /// Extra response headers, if any were added.
    pub fn headers(&self) -> Option<&HeaderMap> {
        self.extras.as_ref().map(|e| &e.headers)
    }

    /// Extension members, if any were added.
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

    /// The problem document this error renders as (`instance` is filled in by the problem layer).
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

/// With the `anyhow` feature: any `anyhow::Error` becomes a 500 with the detail hidden and the
/// cause logged through `tracing`.
#[cfg(feature = "anyhow")]
impl From<anyhow::Error> for HttpError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = &*e as &dyn std::error::Error, "handler failed");
        HttpError::internal("Internal Server Error")
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
        // A string segment without its JSON quotes, an index as its digits.
        let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), String::from);
        let mut parts = self.loc.iter();
        let location = parts.next().map(text).unwrap_or_default();
        let pointer = parts
            .map(|v| format!("/{}", text(v).replace('~', "~0").replace('/', "~1")))
            .collect::<String>();
        (location, pointer)
    }

    /// This failure as an entry of the problem's `errors` member.
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

/// A failed deserialization or [`garde`] validation. Always a 422.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    /// Every failed check.
    pub errors: Vec<ValidationErrorItem>,
}

impl ValidationError {
    /// A `422` carrying `errors`.
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

    /// The `422` problem document, with one `errors` entry per failed check.
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
#[non_exhaustive]
pub enum Rejection {
    /// Deserialization or validation failed: `422`.
    Validation(ValidationError),
    /// Any other failure (missing content type, bad credentials): its own status.
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

// ---- OpenAPI ------------------------------------------------------------------------------

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

    /// Counts `error` events; enough to see that a log record was written.
    #[derive(Clone, Default)]
    struct Errors(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl tracing::Subscriber for Errors {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::ERROR {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn out_of_range_status_is_a_logged_500_not_a_panic() {
        let errors = Errors::default();
        tracing::subscriber::with_default(errors.clone(), || {
            for code in [0u16, 99, 600, 999, 1000, u16::MAX] {
                let error = HttpError::new(code, "upstream said so");
                assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR, "{code}");
                let response = error.into_response();
                assert_eq!(
                    response.status(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "{code}"
                );
            }
        });
        assert_eq!(errors.0.load(std::sync::atomic::Ordering::SeqCst), 6);
        assert_eq!(418u16.into_status(), StatusCode::IM_A_TEAPOT);
        assert_eq!(599u16.into_status().as_u16(), 599);
    }

    #[test]
    fn http_error_stays_small() {
        assert!(
            std::mem::size_of::<HttpError>() <= 48,
            "{}",
            std::mem::size_of::<HttpError>()
        );
    }

    #[tokio::test]
    async fn rendering_without_a_context_is_rfc_9457_and_not_final() {
        let response = HttpError::not_found("gone").into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[header::CONTENT_TYPE], PROBLEM_JSON);
        assert!(
            response.extensions().get::<ProblemRendered>().is_none(),
            "`instance` is still missing, so the layer must finish this one"
        );
        let problem = body_json(response).await;
        assert_eq!(problem["instance"], Value::Null);
    }

    #[tokio::test]
    async fn a_context_fills_in_the_instance_once() {
        let response =
            render_in("/users/42", || HttpError::not_found("gone").into_response()).await;
        assert!(response.extensions().get::<ProblemRendered>().is_some());
        let problem = body_json(response).await;
        assert_eq!(problem["instance"], "/users/42");
        assert_eq!(problem["detail"], "gone");
    }

    #[tokio::test]
    async fn an_instance_set_by_the_user_is_kept() {
        let response = render_in("/users/42", || {
            let mut problem = HttpError::not_found("gone").into_problem();
            problem.instance = Some("urn:uuid:1234".to_string());
            problem.into_response()
        })
        .await;
        assert_eq!(body_json(response).await["instance"], "urn:uuid:1234");
    }

    /// Render `build` as if [`crate::layers::ProblemLayer`] were running around `path`.
    async fn render_in(path: &str, build: impl FnOnce() -> Response) -> Response {
        let context = RenderContext {
            uri: path.parse().unwrap(),
        };
        RENDER.scope(context, async move { build() }).await
    }

    async fn body_json(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
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

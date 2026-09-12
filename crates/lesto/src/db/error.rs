//! `lesto::db::Error`: what a store method can fail with, and how it answers over HTTP.

use std::fmt;

use crate::axum::response::{IntoResponse, Response};
use crate::http::StatusCode;
use crate::{HttpError, OperationBuilder, OperationOutput};
use sqlx::error::ErrorKind;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Error of a store method. Implements `IntoResponse` (RFC 9457 problem) and
/// `OperationOutput`, so `Result<Json<T>, lesto::db::Error>` is a documented handler return type.
///
/// | Variant | Response |
/// |---|---|
/// | `Forbidden` | 403, extension `required_permission` |
/// | `Sqlx(RowNotFound)` | 404 |
/// | `Sqlx(Database)` unique / foreign key violation | 409 |
/// | other `Sqlx`, `Internal` | 500, cause logged with `tracing`, detail hidden |
/// | `Http(e)` | `e`'s own response |
pub enum Error {
    /// The principal lacks `permission`.
    Forbidden { permission: &'static str },
    /// The database said no.
    Sqlx(sqlx::Error),
    /// An HTTP error decided inside the closure (`HttpError::not_found(..)?`).
    Http(HttpError),
    /// Anything else: answered as 500, kept for the log.
    Internal(BoxError),
}

impl Error {
    /// Wrap any error as `Internal` (500).
    pub fn internal(error: impl Into<BoxError>) -> Self {
        Error::Internal(error.into())
    }

    /// Answer with an arbitrary [`HttpError`].
    pub fn http(error: HttpError) -> Self {
        Error::Http(error)
    }

    /// 400 with `detail`.
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Error::Http(HttpError::bad_request(detail))
    }

    /// 404 with `detail` (a `RowNotFound` from sqlx already answers 404 on its own).
    pub fn not_found(detail: impl Into<String>) -> Self {
        Error::Http(HttpError::not_found(detail))
    }

    /// 409 with `detail` (unique and foreign key violations already answer 409 on their own).
    pub fn conflict(detail: impl Into<String>) -> Self {
        Error::Http(HttpError::conflict(detail))
    }

    /// The HTTP status this error answers with.
    pub fn status(&self) -> StatusCode {
        match self {
            Error::Forbidden { .. } => StatusCode::FORBIDDEN,
            Error::Sqlx(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND,
            Error::Sqlx(sqlx::Error::Database(db)) => match db.kind() {
                ErrorKind::UniqueViolation | ErrorKind::ForeignKeyViolation => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            Error::Sqlx(_) | Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Http(e) => e.status,
        }
    }

    /// The problem this error answers with. Logs 500s.
    pub fn into_http_error(self) -> HttpError {
        match self {
            Error::Forbidden { permission } => {
                HttpError::forbidden(format!("Missing permission `{permission}`"))
                    .with_extension("required_permission", permission)
            }
            Error::Sqlx(sqlx::Error::RowNotFound) => HttpError::not_found("Not found"),
            Error::Sqlx(sqlx::Error::Database(db)) => match db.kind() {
                ErrorKind::UniqueViolation => HttpError::conflict("Already exists"),
                ErrorKind::ForeignKeyViolation => {
                    HttpError::conflict("Referenced row does not exist")
                }
                _ => internal(&sqlx::Error::Database(db)),
            },
            Error::Sqlx(e) => internal(&e),
            Error::Internal(e) => internal(&*e),
            Error::Http(e) => e,
        }
    }
}

fn internal(cause: &(dyn std::error::Error + 'static)) -> HttpError {
    tracing::error!(error = cause, "store method failed");
    HttpError::internal("Internal Server Error")
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Forbidden { permission } => f
                .debug_struct("Forbidden")
                .field("permission", permission)
                .finish(),
            Error::Sqlx(e) => f.debug_tuple("Sqlx").field(e).finish(),
            Error::Http(e) => f.debug_tuple("Http").field(e).finish(),
            Error::Internal(e) => f.debug_tuple("Internal").field(e).finish(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Forbidden { permission } => write!(f, "missing permission `{permission}`"),
            Error::Sqlx(e) => write!(f, "database error: {e}"),
            Error::Http(e) => write!(f, "{} {}", e.status, e.detail),
            Error::Internal(e) => write!(f, "internal error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlx(e) => Some(e),
            Error::Internal(e) => Some(&**e),
            Error::Forbidden { .. } | Error::Http(_) => None,
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Error::Sqlx(e)
    }
}

impl From<HttpError> for Error {
    fn from(e: HttpError) -> Self {
        Error::Http(e)
    }
}

impl From<BoxError> for Error {
    fn from(e: BoxError) -> Self {
        Error::Internal(e)
    }
}

#[cfg(feature = "anyhow")]
impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Internal(e.into())
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        self.into_http_error().into_response()
    }
}

impl OperationOutput for Error {
    fn describe(builder: &mut OperationBuilder<'_>, _status: u16) {
        builder.error_response(403, "Forbidden");
        builder.error_response(404, "Not Found");
        builder.error_response(409, "Conflict");
        builder.error_response("default", "Error");
    }
}

/// `result.internal()?` inside a store closure: any error becomes `Error::Internal` (500).
pub trait ResultExt<T> {
    fn internal(self) -> Result<T, Error>;
}

impl<T, E: std::error::Error + Send + Sync + 'static> ResultExt<T> for Result<T, E> {
    fn internal(self) -> Result<T, Error> {
        self.map_err(Error::internal)
    }
}

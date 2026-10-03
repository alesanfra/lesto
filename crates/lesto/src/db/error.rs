//! `lesto::db::Error`: what a store method can fail with, and how it answers over HTTP.

use std::fmt;

use crate::axum::response::{IntoResponse, Response};
use crate::http::{StatusCode, header};
use crate::{HttpError, OperationBuilder, OperationOutput};
use sqlx::error::ErrorKind;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Error of a store method. Implements `IntoResponse` (RFC 9457 problem) and
/// `OperationOutput`, so `Result<Json<T>, lesto::db::Error>` is a documented handler return type.
///
/// | Variant | Response |
/// |---|---|
/// | `Forbidden` | 403, extension `required_permission` |
/// | `Sqlx(Database)` unique / foreign key violation | 409 |
/// | `Sqlx(Database)` transient conflict ([`is_transient_conflict`](Self::is_transient_conflict)) | 409, `Retry-After: 0` |
/// | other `Sqlx` (including `RowNotFound`), `Internal` | 500, cause logged with `tracing`, detail hidden |
/// | `Http(e)` | `e`'s own response |
///
/// A `RowNotFound` is a 500 on purpose: a `fetch_one` that finds nothing on a secondary lookup is
/// a bug, and a 404 would hide it. Say which lookups mean "not found" with
/// [`NotFoundExt::or_not_found`].
#[non_exhaustive]
pub enum Error {
    /// The principal lacks `permission`.
    Forbidden {
        /// The permission that was required.
        permission: &'static str,
    },
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

    /// 404 with `detail`. See also [`NotFoundExt::or_not_found`].
    pub fn not_found(detail: impl Into<String>) -> Self {
        Error::Http(HttpError::not_found(detail))
    }

    /// 409 with `detail` (unique and foreign key violations already answer 409 on their own).
    pub fn conflict(detail: impl Into<String>) -> Self {
        Error::Http(HttpError::conflict(detail))
    }

    /// Did the transaction simply lose a race — a serialization failure, a deadlock, or
    /// SQLite's `SQLITE_BUSY`?
    ///
    /// `true` means re-running the same closure is a sensible thing to do, and is what
    /// [`Store::read_with`](crate::db::Store::read_with) and
    /// [`write_with`](crate::db::Store::write_with) check before retrying. Lock *timeouts* are
    /// deliberately not in this set: a retry does not fix one.
    pub fn is_transient_conflict(&self) -> bool {
        match self {
            Error::Sqlx(sqlx::Error::Database(db)) => is_transient_conflict(&**db),
            _ => false,
        }
    }

    /// The HTTP status this error answers with.
    pub fn status(&self) -> StatusCode {
        match self {
            Error::Forbidden { .. } => StatusCode::FORBIDDEN,
            Error::Sqlx(sqlx::Error::Database(db)) if is_transient_conflict(&**db) => {
                StatusCode::CONFLICT
            }
            Error::Sqlx(sqlx::Error::Database(db)) => match db.kind() {
                ErrorKind::UniqueViolation | ErrorKind::ForeignKeyViolation => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            Error::Sqlx(_) | Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Http(e) => e.status(),
        }
    }

    /// The problem this error answers with. Logs 500s.
    pub fn into_http_error(self) -> HttpError {
        match self {
            Error::Forbidden { permission } => {
                HttpError::forbidden(format!("Missing permission `{permission}`"))
                    .with_extension("required_permission", permission)
            }
            Error::Sqlx(sqlx::Error::Database(db)) if is_transient_conflict(&*db) => {
                // Not logged as an error: under `Isolation::Serializable` this is the database
                // doing its job, and the answer tells the client what to do about it.
                tracing::debug!(
                    code = db.code().as_deref(),
                    "transaction conflicted; answering 409"
                );
                HttpError::conflict("Conflicting concurrent change, retry the request")
                    // Delta-seconds 0: the conflict is instantaneous, so retrying at once is
                    // the right thing — there is nothing to wait for.
                    .with_header(header::RETRY_AFTER, "0")
            }
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

/// A conflict the client can simply retry: the transaction lost a race, and nothing about the
/// request itself was wrong.
///
/// Covered: `40001` serialization failure (Postgres, and where MySQL maps `ER_LOCK_DEADLOCK`),
/// `40P01` deadlock detected (Postgres), and SQLite's `SQLITE_BUSY` family.
///
/// **Not** covered: lock wait timeouts (MySQL `ER_LOCK_WAIT_TIMEOUT`, Postgres `55P03`). Those
/// mean somebody held a lock too long, which a retry does not fix and which should be looked
/// at, so they stay 500s.
fn is_transient_conflict(db: &dyn sqlx::error::DatabaseError) -> bool {
    let Some(code) = db.code() else {
        return false;
    };
    match code.as_ref() {
        "40001" | "40P01" => true,
        // SQLite reports the *extended* result code as a number: the low byte is the primary
        // code and `SQLITE_BUSY` is 5, so 5, 261 (`_RECOVERY`), 517 (`_SNAPSHOT`) and 773
        // (`_TIMEOUT`) all qualify, as would a future one. Bounded below 10000 because a
        // Postgres or MySQL SQLSTATE is five characters and an all-digit one could otherwise
        // land on the same remainder by accident; every SQLite code is far smaller.
        other => other
            .parse::<i32>()
            .is_ok_and(|code| code < 10_000 && code % 256 == 5),
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
            Error::Http(e) => write!(f, "{} {}", e.status(), e.detail()),
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
    /// Map the error to [`Error::Internal`].
    fn internal(self) -> Result<T, Error>;
}

impl<T, E: std::error::Error + Send + Sync + 'static> ResultExt<T> for Result<T, E> {
    fn internal(self) -> Result<T, Error> {
        self.map_err(Error::internal)
    }
}

/// `.or_not_found("no such note")`: the lookups where "no row" means `404`.
///
/// lesto does not turn a bare `sqlx::Error::RowNotFound` into a 404 (see [`Error`]). Call this on
/// the one query whose absence is the answer:
///
/// ```ignore
/// sqlx::query_as("SELECT id, text FROM notes WHERE id = ?")
///     .bind(id)
///     .fetch_one(conn)
///     .await
///     .or_not_found("no such note")
/// ```
///
/// On an `Option` (`fetch_optional`), `None` becomes the 404.
pub trait NotFoundExt<T> {
    /// `404` with `detail` when there is no row; any other error is kept.
    fn or_not_found(self, detail: impl Into<String>) -> Result<T, Error>;
}

impl<T> NotFoundExt<T> for Result<T, sqlx::Error> {
    fn or_not_found(self, detail: impl Into<String>) -> Result<T, Error> {
        self.map_err(|e| match e {
            sqlx::Error::RowNotFound => Error::not_found(detail),
            e => Error::Sqlx(e),
        })
    }
}

impl<T> NotFoundExt<T> for Option<T> {
    fn or_not_found(self, detail: impl Into<String>) -> Result<T, Error> {
        self.ok_or_else(|| Error::not_found(detail))
    }
}

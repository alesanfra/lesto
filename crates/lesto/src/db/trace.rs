//! Store spans, named and attributed after the OpenTelemetry semantic conventions for
//! [database clients](https://opentelemetry.io/docs/specs/semconv/database/database-spans/).
//!
//! Every transaction a store opens — `read`, `read_with`, `write`, `write_with` — runs inside a
//! `CLIENT` span that starts with the `BEGIN` and ends with the commit or the rollback, so the
//! time a request spends in the database (and the retries it went through) sits under the
//! request span of [`crate::trace`] without the store methods being annotated.
//!
//! | field | value |
//! |---|---|
//! | `otel.name`, `db.operation.name` | the statement the transaction opens with: `BEGIN`, `BEGIN READ ONLY`, `START TRANSACTION`, `BEGIN DEFERRED` — whatever the [`Dialect`] says |
//! | `db.system.name` | `postgresql`, `mysql`, `sqlite` |
//! | `error.type`, `db.response.status_code` | on failure: the database's own code (`23505`, `2067`), else the kind of [`Error`] |
//!
//! There is no `db.query.text`: the statement lesto builds carries the principal's
//! [`transaction_settings`](crate::db::PrincipalDocs::transaction_settings), which is identity
//! data, and the queries themselves belong to the closure, which lesto never sees. Spans for
//! individual statements are sqlx's business, not lesto's.
//!
//! A requirement that fails gets no span: a `403` never reaches the database.

use std::borrow::Cow;
use std::future::Future;

use tracing::field::Empty;
use tracing::{Instrument, Span};

use crate::db::dialect::Dialect;
use crate::db::error::Error;

/// The `CLIENT` span for one transaction on `DB`.
pub(crate) fn transaction_span<DB: Dialect>(read_only: bool) -> Span {
    let span = tracing::info_span!(
        "db.client.operation",
        otel.name = Empty,
        otel.kind = "client",
        otel.status_code = Empty,
        db.system.name = DB::SYSTEM,
        db.operation.name = Empty,
        db.response.status_code = Empty,
        error.type = Empty,
    );
    if span.is_disabled() {
        return span;
    }
    // `tracing` span names are constants, so the name of the operation goes in `otel.name`,
    // which is where `tracing-opentelemetry` reads it from.
    let operation = DB::begin_keyword(read_only);
    span.record("otel.name", operation);
    span.record("db.operation.name", operation);
    span
}

/// Run `future` inside `span`, recording how it ended.
pub(crate) async fn traced<T, F>(span: Span, future: F) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>>,
{
    let result = future.instrument(span.clone()).await;
    if let Err(error) = &result {
        span.record("error.type", error_type(error).as_ref());
        span.record("otel.status_code", "ERROR");
        if let Some(code) = database_code(error) {
            span.record("db.response.status_code", code.as_ref());
        }
    }
    result
}

/// `error.type`: the database's own code when there is one, else the kind of failure.
fn error_type(error: &Error) -> Cow<'_, str> {
    match error {
        Error::Forbidden { .. } => Cow::Borrowed("Forbidden"),
        Error::Sqlx(sqlx::Error::RowNotFound) => Cow::Borrowed("RowNotFound"),
        Error::Sqlx(sqlx::Error::Database(_)) => {
            database_code(error).unwrap_or(Cow::Borrowed("DatabaseError"))
        }
        Error::Sqlx(_) => Cow::Borrowed("Sqlx"),
        Error::Http(e) => Cow::Owned(e.status.as_u16().to_string()),
        Error::Internal(_) => Cow::Borrowed("Internal"),
    }
}

/// `db.response.status_code`: the SQLSTATE (Postgres), the error number (MySQL) or the extended
/// result code (SQLite), as the driver reports it.
fn database_code(error: &Error) -> Option<Cow<'_, str>> {
    match error {
        Error::Sqlx(sqlx::Error::Database(db)) => db.code(),
        _ => None,
    }
}

//! Store spans, named and attributed after the OpenTelemetry semantic conventions for
//! [database clients](https://opentelemetry.io/docs/specs/semconv/database/database-spans/).
//!
//! Every transaction a store opens — `read`, `read_with`, `write`, `write_with` — runs inside a
//! `CLIENT` span that starts with the `BEGIN` and ends with the commit or the rollback, so the
//! time a request spends in the database (and the retries it went through) sits under the
//! request span of [`crate::trace`] without the store methods being annotated.
//!
//! The span is **named after the store method that opened it** (`NoteStore::list`,
//! `NoteStore::create`), which is the thing a trace view has to show: `BEGIN` is a statement,
//! not an operation. The name is read from the type name of the closure passed to `read` or
//! `write`, which the compiler spells with the path of the function the closure was written in.
//! When that path says nothing (a closure built somewhere else and passed in), the name falls
//! back to `read` or `write`.
//!
//! Three fields on success, because a trace with twenty attributes per span is a trace nobody
//! reads:
//!
//! | field | value |
//! |---|---|
//! | `otel.name` | the store method, e.g. `NoteStore::list` |
//! | `otel.kind` | `client` |
//! | `db.system.name` | `postgresql`, `mysql`, `sqlite` |
//!
//! and on failure `error.type` plus `db.response.status_code`, the database's own code (`23505`,
//! `2067`) when it has one.
//!
//! There is no `db.operation.name`: the conventions ask for it only when it is readily
//! available, and lesto does not see the statements — the closure does. There is no
//! `db.query.text` either: the statement lesto builds carries the principal's
//! [`transaction_settings`](crate::db::PrincipalDocs::transaction_settings), which is identity
//! data. Per-statement spans are sqlx's business; `RUST_LOG=sqlx::query=debug` puts each query,
//! with its timing, inside the span of the method that ran it.
//!
//! A requirement that fails gets no span: a `403` never reaches the database.

use std::borrow::Cow;
use std::future::Future;

use tracing::field::Empty;
use tracing::{Instrument, Span};

use crate::db::dialect::Dialect;
use crate::db::error::Error;
pub(crate) use crate::trace::operation_of;

/// The `CLIENT` span for one transaction on `DB`, named after `operation`.
pub(crate) fn transaction_span<DB: Dialect>(operation: &str) -> Span {
    let span = tracing::info_span!(
        "db.client.operation",
        otel.name = Empty,
        otel.kind = "client",
        otel.status_code = Empty,
        db.system.name = DB::SYSTEM,
        db.response.status_code = Empty,
        error.type = Empty,
    );
    if span.is_disabled() {
        return span;
    }
    // `tracing` span names are constants, so the name of the operation goes in `otel.name`,
    // which is where `tracing-opentelemetry` reads it from.
    span.record("otel.name", operation);
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
        Error::Http(e) => Cow::Owned(e.status().as_u16().to_string()),
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

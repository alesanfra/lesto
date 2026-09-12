//! Per-database knobs. One impl per enabled feature (`postgres`, `mysql`, `sqlite`).

mod sealed {
    pub trait Sealed {}
}

/// A database the `db` feature of lesto knows how to open a read-only transaction on.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a database lesto supports",
    label = "no `Dialect` for this database",
    note = "enable the matching feature of `lesto`: `postgres`, `mysql` or `sqlite`"
)]
pub trait Dialect: sqlx::Database + sealed::Sealed {
    /// Statement that opens a read-only transaction, passed to `Pool::begin_with`.
    const BEGIN_READ_ONLY: &'static str;
}

#[cfg(feature = "postgres")]
impl sealed::Sealed for sqlx::Postgres {}
#[cfg(feature = "postgres")]
impl Dialect for sqlx::Postgres {
    const BEGIN_READ_ONLY: &'static str = "BEGIN READ ONLY";
}

#[cfg(feature = "mysql")]
impl sealed::Sealed for sqlx::MySql {}
#[cfg(feature = "mysql")]
impl Dialect for sqlx::MySql {
    const BEGIN_READ_ONLY: &'static str = "START TRANSACTION READ ONLY";
}

/// SQLite has no read-only transactions; a deferred transaction is the closest thing.
#[cfg(feature = "sqlite")]
impl sealed::Sealed for sqlx::Sqlite {}
#[cfg(feature = "sqlite")]
impl Dialect for sqlx::Sqlite {
    const BEGIN_READ_ONLY: &'static str = "BEGIN DEFERRED";
}

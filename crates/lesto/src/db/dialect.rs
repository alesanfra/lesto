//! Per-database knobs. One impl per enabled feature (`postgres`, `mysql`, `sqlite`).

use std::borrow::Cow;

use crate::db::error::Error;
use crate::db::isolation::Isolation;
use crate::db::settings::TransactionSettings;

/// A database the `db` feature of lesto knows how to open a read-only transaction on.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a database lesto supports",
    label = "no `Dialect` for this database",
    note = "enable the matching feature of `lesto`: `postgres`, `mysql` or `sqlite`"
)]
pub trait Dialect: sqlx::Database + sealed::Sealed {
    /// Statement that opens a read-only transaction, passed to `Pool::begin_with`.
    const BEGIN_READ_ONLY: &'static str;

    /// Statement that opens a read-write transaction.
    const BEGIN: &'static str;

    /// The clause that asks for `isolation`, or `None` when this database reaches it another
    /// way (SQLite, through the kind of `BEGIN` it opens).
    fn isolation_clause(isolation: Isolation) -> Option<&'static str>;

    /// The statement `Store::read` / `Store::write` opens the transaction with: a `BEGIN`, the
    /// isolation it was asked for, and — where the database has transaction-local settings —
    /// one statement per entry of `settings`, so the whole preamble is one round trip.
    ///
    /// The default implementation puts the isolation clause inside the `BEGIN`, which is the
    /// Postgres and SQLite shape; MySQL overrides it because `START TRANSACTION` takes no
    /// isolation clause. It ignores an empty `settings` and refuses a non-empty one: only
    /// Postgres has a setting whose lifetime is the transaction. See the
    /// [`settings`](crate::db::settings) module.
    fn begin_statement(
        read_only: bool,
        isolation: Isolation,
        settings: &TransactionSettings,
    ) -> Result<Cow<'static, str>, Error> {
        let entries = settings.entries()?;
        if !entries.is_empty() {
            return Err(Error::internal(Unsupported(std::any::type_name::<Self>())));
        }
        Ok(begin(
            Self::begin_keyword(read_only),
            isolation_of::<Self>(isolation),
        ))
    }

    /// `BEGIN` or `BEGIN READ ONLY`, before the isolation clause is folded in.
    fn begin_keyword(read_only: bool) -> &'static str {
        if read_only {
            Self::BEGIN_READ_ONLY
        } else {
            Self::BEGIN
        }
    }
}

/// The isolation clause for `DB`, or `""` when it needs none.
fn isolation_of<DB: Dialect>(isolation: Isolation) -> &'static str {
    DB::isolation_clause(isolation).unwrap_or("")
}

/// `BEGIN [READ ONLY]` with the isolation clause spliced in after the keyword, which is where
/// both Postgres and SQLite want it.
fn begin(keyword: &'static str, clause: &str) -> Cow<'static, str> {
    if clause.is_empty() {
        return Cow::Borrowed(keyword);
    }
    match keyword.split_once(' ') {
        // `BEGIN READ ONLY` → `BEGIN ISOLATION LEVEL .. READ ONLY`.
        Some((first, rest)) => Cow::Owned(format!("{first} {clause} {rest}")),
        None => Cow::Owned(format!("{keyword} {clause}")),
    }
}

/// The cause logged when a principal publishes transaction settings on a database that has
/// none.
#[derive(Debug)]
struct Unsupported(&'static str);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` has no transaction-local settings, so `Authenticated::transaction_settings` \
             cannot be honored on it: return `TransactionSettings::empty()`, or move the \
             identity into the queries themselves",
            self.0
        )
    }
}

impl std::error::Error for Unsupported {}

mod sealed {
    pub trait Sealed {}
}

#[cfg(feature = "postgres")]
impl sealed::Sealed for sqlx::Postgres {}
#[cfg(feature = "postgres")]
impl Dialect for sqlx::Postgres {
    const BEGIN_READ_ONLY: &'static str = "BEGIN READ ONLY";
    const BEGIN: &'static str = "BEGIN";

    fn isolation_clause(isolation: Isolation) -> Option<&'static str> {
        match isolation {
            Isolation::Default => None,
            Isolation::Snapshot => Some("ISOLATION LEVEL REPEATABLE READ"),
            Isolation::Serializable => Some("ISOLATION LEVEL SERIALIZABLE"),
        }
    }

    /// `BEGIN` and the `SET LOCAL`s in one simple-query string.
    ///
    /// Every value is interpolated, because `SET LOCAL` accepts no bind parameters — which is
    /// why a value can only be a
    /// [`Literal`](crate::db::settings::Literal), whose text form cannot contain a quote, and
    /// a name can only be a checked `&'static str`. That is the audit
    /// `sqlx::AssertSqlSafe` asks for at the call site in
    /// [`Store`](struct@crate::db::Store).
    ///
    /// `BEGIN` stays the first statement because this function writes it; the caller only ever
    /// supplies name/value pairs. sqlx then checks that the connection really is in a
    /// transaction afterwards (`Error::BeginFailed`), so the failure mode where the settings
    /// evaporate outside a transaction and every policy quietly resolves to its anonymous
    /// branch cannot happen silently.
    fn begin_statement(
        read_only: bool,
        isolation: Isolation,
        settings: &TransactionSettings,
    ) -> Result<Cow<'static, str>, Error> {
        let entries = settings.entries()?;
        let begin = begin(
            Self::begin_keyword(read_only),
            isolation_of::<Self>(isolation),
        );
        if entries.is_empty() {
            return Ok(begin);
        }
        // `SET LOCAL <name> = '<value>'` is around 40 characters with a UUID in it.
        let mut sql = String::with_capacity(begin.len() + entries.len() * 48);
        sql.push_str(&begin);
        for (name, value) in entries {
            sql.push_str("; SET LOCAL ");
            sql.push_str(name);
            sql.push_str(" = '");
            if let Some(value) = value {
                value.render_into(&mut sql);
            }
            sql.push('\'');
        }
        Ok(Cow::Owned(sql))
    }
}

#[cfg(feature = "mysql")]
impl sealed::Sealed for sqlx::MySql {}
#[cfg(feature = "mysql")]
impl Dialect for sqlx::MySql {
    const BEGIN_READ_ONLY: &'static str = "START TRANSACTION READ ONLY";
    const BEGIN: &'static str = "START TRANSACTION";

    fn isolation_clause(isolation: Isolation) -> Option<&'static str> {
        match isolation {
            Isolation::Default => None,
            Isolation::Snapshot => Some("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ"),
            Isolation::Serializable => Some("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE"),
        }
    }

    /// `START TRANSACTION` accepts no isolation clause, so the clause is a statement of its own
    /// and has to come **first**. With no scope it applies to the next transaction only, which
    /// is exactly this one — nothing is left set on the pooled connection.
    fn begin_statement(
        read_only: bool,
        isolation: Isolation,
        settings: &TransactionSettings,
    ) -> Result<Cow<'static, str>, Error> {
        let entries = settings.entries()?;
        if !entries.is_empty() {
            return Err(Error::internal(Unsupported(std::any::type_name::<Self>())));
        }
        let keyword = Self::begin_keyword(read_only);
        Ok(match Self::isolation_clause(isolation) {
            None => Cow::Borrowed(keyword),
            Some(clause) => Cow::Owned(format!("{clause}; {keyword}")),
        })
    }
}

/// SQLite has no read-only transactions; a deferred transaction is the closest thing.
#[cfg(feature = "sqlite")]
impl sealed::Sealed for sqlx::Sqlite {}
#[cfg(feature = "sqlite")]
impl Dialect for sqlx::Sqlite {
    const BEGIN_READ_ONLY: &'static str = "BEGIN DEFERRED";
    const BEGIN: &'static str = "BEGIN";

    /// No isolation levels to ask for: SQLite has one writer at a time, which is serializable
    /// already. `Serializable` instead changes *when* the write lock is taken — see
    /// [`begin_statement`](Self::begin_statement).
    fn isolation_clause(_isolation: Isolation) -> Option<&'static str> {
        None
    }

    /// `Serializable` on a read-write transaction opens `BEGIN IMMEDIATE`: the write lock is
    /// taken at the start, so a check-then-act cannot fail with `SQLITE_BUSY` when it reaches
    /// its first write. A read-only transaction is already a snapshot, so it stays `DEFERRED`.
    fn begin_statement(
        read_only: bool,
        isolation: Isolation,
        settings: &TransactionSettings,
    ) -> Result<Cow<'static, str>, Error> {
        let entries = settings.entries()?;
        if !entries.is_empty() {
            return Err(Error::internal(Unsupported(std::any::type_name::<Self>())));
        }
        Ok(Cow::Borrowed(match (read_only, isolation) {
            (false, Isolation::Serializable) => "BEGIN IMMEDIATE",
            (true, _) => Self::BEGIN_READ_ONLY,
            (false, _) => Self::BEGIN,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "postgres")]
    fn pg(read_only: bool, isolation: Isolation, settings: &TransactionSettings) -> String {
        <sqlx::Postgres as Dialect>::begin_statement(read_only, isolation, settings)
            .unwrap()
            .into_owned()
    }

    #[cfg(feature = "sqlite")]
    fn sqlite(read_only: bool, isolation: Isolation) -> String {
        <sqlx::Sqlite as Dialect>::begin_statement(
            read_only,
            isolation,
            &TransactionSettings::empty(),
        )
        .unwrap()
        .into_owned()
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_without_settings_is_the_bare_begin() {
        let empty = TransactionSettings::empty();
        assert_eq!(pg(true, Isolation::Default, &empty), "BEGIN READ ONLY");
        assert_eq!(pg(false, Isolation::Default, &empty), "BEGIN");
        // And it does not allocate.
        assert!(matches!(
            <sqlx::Postgres as Dialect>::begin_statement(true, Isolation::Default, &empty).unwrap(),
            Cow::Borrowed(_)
        ));
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_renders_one_set_local_per_entry() {
        let settings = TransactionSettings::empty()
            .set("app.user_id", uuid::Uuid::nil())
            .set_opt("app.organization_id", None::<uuid::Uuid>)
            .set("app.is_staff", false)
            .set("app.tenant", 7i64);
        assert_eq!(
            pg(true, Isolation::Default, &settings),
            "BEGIN READ ONLY\
             ; SET LOCAL app.user_id = '00000000-0000-0000-0000-000000000000'\
             ; SET LOCAL app.organization_id = ''\
             ; SET LOCAL app.is_staff = 'false'\
             ; SET LOCAL app.tenant = '7'"
        );
    }

    /// The isolation clause goes right after `BEGIN`, before `READ ONLY`: both are transaction
    /// modes of the same statement and Postgres wants the keyword first.
    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_folds_the_isolation_clause_into_the_begin() {
        let empty = TransactionSettings::empty();
        assert_eq!(
            pg(true, Isolation::Snapshot, &empty),
            "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY"
        );
        assert_eq!(
            pg(false, Isolation::Serializable, &empty),
            "BEGIN ISOLATION LEVEL SERIALIZABLE"
        );
        assert_eq!(
            pg(true, Isolation::Serializable, &empty),
            "BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY"
        );
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_combines_isolation_and_settings() {
        let settings = TransactionSettings::empty().set("app.user_id", 1i64);
        assert_eq!(
            pg(false, Isolation::Serializable, &settings),
            "BEGIN ISOLATION LEVEL SERIALIZABLE; SET LOCAL app.user_id = '1'"
        );
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_begin_is_always_the_first_statement() {
        let settings = TransactionSettings::empty().set("app.user_id", 1i64);
        for read_only in [true, false] {
            for isolation in [
                Isolation::Default,
                Isolation::Snapshot,
                Isolation::Serializable,
            ] {
                let sql = pg(read_only, isolation, &settings);
                let first = sql.split(';').next().unwrap();
                assert!(first.starts_with("BEGIN"), "{sql}");
                assert_eq!(sql.matches("BEGIN").count(), 1, "{sql}");
            }
        }
    }

    /// `START TRANSACTION` takes no isolation clause, so it is a statement of its own — and it
    /// has to come first, which is the opposite order from Postgres.
    #[cfg(feature = "mysql")]
    #[test]
    fn mysql_puts_the_isolation_clause_before_the_start() {
        let empty = TransactionSettings::empty();
        let mysql = |read_only, isolation| {
            <sqlx::MySql as Dialect>::begin_statement(read_only, isolation, &empty)
                .unwrap()
                .into_owned()
        };
        assert_eq!(mysql(false, Isolation::Default), "START TRANSACTION");
        assert_eq!(
            mysql(true, Isolation::Default),
            "START TRANSACTION READ ONLY"
        );
        assert_eq!(
            mysql(false, Isolation::Serializable),
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE; START TRANSACTION"
        );
        assert_eq!(
            mysql(true, Isolation::Snapshot),
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ; START TRANSACTION READ ONLY"
        );
    }

    /// SQLite asks for nothing: what changes is when the write lock is taken.
    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_takes_the_write_lock_up_front_for_serializable() {
        assert_eq!(sqlite(true, Isolation::Default), "BEGIN DEFERRED");
        assert_eq!(sqlite(false, Isolation::Default), "BEGIN");
        assert_eq!(sqlite(false, Isolation::Snapshot), "BEGIN");
        assert_eq!(sqlite(false, Isolation::Serializable), "BEGIN IMMEDIATE");
        // A read is already a snapshot; it stays deferred.
        assert_eq!(sqlite(true, Isolation::Serializable), "BEGIN DEFERRED");
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_refuses_settings_it_cannot_honor() {
        let settings = TransactionSettings::empty().set("app.user_id", 1i64);
        let error = <sqlx::Sqlite as Dialect>::begin_statement(true, Isolation::Default, &settings)
            .unwrap_err();
        assert_eq!(
            error.status(),
            crate::http::StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}

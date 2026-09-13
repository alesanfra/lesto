//! `Store<M, P, DB>`: a principal, a database handle, and `read`/`write` that run a closure
//! inside a transaction after checking a requirement.

use std::borrow::Cow;
use std::future::Future;
use std::marker::PhantomData;

use crate::axum::extract::{FromRef, FromRequestParts};
use crate::axum::response::Response;
use crate::http::request::Parts;
use crate::{OperationBuilder, OperationInput};
use sqlx::{AssertSqlSafe, Database, Transaction};

use crate::db::dialect::Dialect;
use crate::db::error::Error;
use crate::db::handle::Db;
use crate::db::isolation::Isolation;
use crate::db::mode::{Mode, Writable};
use crate::db::principal::{Principal, PrincipalDocs};
use crate::db::requirement::Requirement;
use crate::db::settings::TransactionSettings;
use crate::db::trace;

/// The inner type of every store. Wrap it in a newtype and `#[derive(Store)]`:
///
/// ```ignore
/// #[derive(lesto::db::Store)]
/// pub struct NoteStore<M, P>(lesto::db::Store<M, P, Sqlite>);
///
/// impl<M: Mode, P> NoteStore<M, P> {
///     pub async fn list(&self) -> Result<Vec<Note>, Error> {
///         self.read(Anyone, async |conn| {
///             sqlx::query_as("SELECT id, author, text FROM notes").fetch_all(conn).await
///         }).await
///     }
/// }
/// ```
pub struct Store<M, P, DB: Database> {
    db: Db<DB>,
    principal: P,
    /// Read off the principal once, when the store is built, rather than per method: they
    /// cannot change while the store lives, and this is what keeps `read` and `write` free of
    /// a `P: PrincipalDocs` bound that every user's store `impl` block would have to repeat.
    settings: TransactionSettings,
    _mode: PhantomData<fn() -> M>,
}

impl<M: Mode, P, DB: Dialect> Store<M, P, DB> {
    /// Build a store by hand (tests, background jobs). Handlers get theirs by extraction.
    pub fn new(db: Db<DB>, principal: P) -> Self
    where
        P: PrincipalDocs,
    {
        let settings = principal.transaction_settings();
        Self {
            db,
            principal,
            settings,
            _mode: PhantomData,
        }
    }

    /// Who holds this store.
    pub fn principal(&self) -> &P {
        &self.principal
    }

    pub fn into_principal(self) -> P {
        self.principal
    }

    /// The pools, for anything the store API does not cover.
    pub fn db(&self) -> &Db<DB> {
        &self.db
    }

    /// Check `requirement`, open a **read-only** transaction (on the replica when configured),
    /// run `f`, commit. Any error rolls back.
    ///
    /// The principal's [`transaction_settings`](PrincipalDocs::transaction_settings) are
    /// published in the same round trip as the `BEGIN`. For a stable snapshot across several
    /// queries, use [`read_with`](Self::read_with).
    pub async fn read<T, E, R, F>(&self, requirement: R, f: F) -> Result<T, Error>
    where
        R: Requirement<P>,
        E: Into<Error>,
        F: for<'c> AsyncFnOnce(&'c mut DB::Connection) -> Result<T, E>,
    {
        requirement.check(&self.principal)?;
        // Not `read_with(.., Isolation::Default, ..)`: that one may run `f` twice and so takes
        // an `AsyncFn`, which is a bound the common case should not have to satisfy. At the
        // database's own isolation there is no conflict to retry anyway.
        trace::traced(
            trace::transaction_span::<DB>(&trace::operation_of::<F>("read")),
            async move {
                let tx = self
                    .db
                    .reads()
                    .begin_with(AssertSqlSafe(self.begin(true, Isolation::Default)?))
                    .await?;
                run(tx, f).await
            },
        )
        .await
    }

    /// [`read`](Self::read), asking the database for `isolation`.
    ///
    /// On a transient conflict the closure is re-run, up to
    /// [`Db::with_conflict_retries`] times; if the last attempt still conflicts the answer is
    /// **409** with `Retry-After: 0` — the request is safe to send again.
    ///
    /// `f` is an `AsyncFn`, not an `AsyncFnOnce`, precisely because it may run more than once:
    /// keep it free of effects the outside world can see (no email, no queue publish), or the
    /// retry sends them twice.
    pub async fn read_with<T, E, R, F>(
        &self,
        requirement: R,
        isolation: Isolation,
        f: F,
    ) -> Result<T, Error>
    where
        R: Requirement<P>,
        E: Into<Error>,
        F: for<'c> AsyncFn(&'c mut DB::Connection) -> Result<T, E>,
    {
        requirement.check(&self.principal)?;
        let statement = self.begin(true, isolation)?;
        trace::traced(
            trace::transaction_span::<DB>(&trace::operation_of::<F>("read")),
            self.attempt(&f, || {
                self.db.reads().begin_with(AssertSqlSafe(statement.clone()))
            }),
        )
        .await
    }

    /// Check `requirement`, open a read-write transaction on the primary, run `f`, commit.
    /// Any error rolls back. Only available when `M: Writable` (`ReadWrite`).
    ///
    /// The principal's [`transaction_settings`](PrincipalDocs::transaction_settings) are
    /// published in the same round trip as the `BEGIN`. When a check inside `f` decides whether
    /// to write, that check does **not** hold to commit at this isolation — use
    /// [`write_with`](Self::write_with) with [`Isolation::Serializable`], or a unique
    /// constraint.
    pub async fn write<T, E, R, F>(&self, requirement: R, f: F) -> Result<T, Error>
    where
        M: Writable,
        R: Requirement<P>,
        E: Into<Error>,
        F: for<'c> AsyncFnOnce(&'c mut DB::Connection) -> Result<T, E>,
    {
        requirement.check(&self.principal)?;
        // `AsyncFnOnce`, and no retry: see `read`.
        trace::traced(
            trace::transaction_span::<DB>(&trace::operation_of::<F>("write")),
            async move {
                let tx = self
                    .db
                    .primary()
                    .begin_with(AssertSqlSafe(self.begin(false, Isolation::Default)?))
                    .await?;
                run(tx, f).await
            },
        )
        .await
    }

    /// [`write`](Self::write), asking the database for `isolation`.
    ///
    /// [`Isolation::Serializable`] is what a check-then-act needs: a row read to decide still
    /// holds when the transaction commits. On a conflict the closure is re-run, up to
    /// [`Db::with_conflict_retries`] times, and a conflict that survives the last attempt
    /// answers **409** with `Retry-After: 0`, not 500.
    ///
    /// `f` is an `AsyncFn`, not an `AsyncFnOnce`, precisely because it may run more than once:
    /// keep it free of effects the outside world can see, or the retry repeats them.
    pub async fn write_with<T, E, R, F>(
        &self,
        requirement: R,
        isolation: Isolation,
        f: F,
    ) -> Result<T, Error>
    where
        M: Writable,
        R: Requirement<P>,
        E: Into<Error>,
        F: for<'c> AsyncFn(&'c mut DB::Connection) -> Result<T, E>,
    {
        requirement.check(&self.principal)?;
        let statement = self.begin(false, isolation)?;
        trace::traced(
            trace::transaction_span::<DB>(&trace::operation_of::<F>("write")),
            self.attempt(&f, || {
                self.db
                    .primary()
                    .begin_with(AssertSqlSafe(statement.clone()))
            }),
        )
        .await
    }

    /// Run `f` in a transaction from `open`, retrying a transient conflict.
    ///
    /// The requirement is checked by the caller, once: a permission cannot change between
    /// attempts, and re-checking it would only make the 403 arrive later.
    async fn attempt<T, E, F, O, Fut>(&self, f: &F, open: O) -> Result<T, Error>
    where
        E: Into<Error>,
        F: for<'c> AsyncFn(&'c mut DB::Connection) -> Result<T, E>,
        O: Fn() -> Fut,
        Fut: Future<Output = Result<Transaction<'static, DB>, sqlx::Error>>,
    {
        let mut attempts_left = self.db.conflict_retries();
        loop {
            let error = match run(open().await?, f).await {
                Ok(value) => return Ok(value),
                Err(e) => e,
            };
            if attempts_left == 0 || !error.is_transient_conflict() {
                return Err(error);
            }
            attempts_left -= 1;
            tracing::debug!(
                attempts_left,
                error = &error as &dyn std::error::Error,
                "transaction conflicted, retrying"
            );
        }
    }

    /// The settings this store publishes to every transaction it opens.
    pub fn settings(&self) -> &TransactionSettings {
        &self.settings
    }

    /// The `BEGIN`, plus this store's transaction settings, as one statement.
    ///
    /// `AssertSqlSafe` is the audit sqlx asks for, and this is where it is owed: the statement
    /// is built by [`Dialect::begin_statement`] from a checked `&'static str` name and a
    /// [`Literal`](crate::db::settings::Literal) whose text form cannot contain a quote, never
    /// from request data. `SET LOCAL` accepts no bind parameters, so there is no alternative to
    /// interpolation — only the choice of what may be interpolated.
    fn begin(&self, read_only: bool, isolation: Isolation) -> Result<Cow<'static, str>, Error> {
        DB::begin_statement(read_only, isolation, &self.settings)
    }
}

async fn run<T, E, DB, F>(mut tx: Transaction<'static, DB>, f: F) -> Result<T, Error>
where
    DB: Database,
    E: Into<Error>,
    F: for<'c> AsyncFnOnce(&'c mut DB::Connection) -> Result<T, E>,
{
    match f(&mut tx).await {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(e) => {
            // The error the caller cares about is `e`; a failed rollback is only logged.
            if let Err(rollback) = tx.rollback().await {
                tracing::warn!(
                    error = &rollback as &dyn std::error::Error,
                    "rollback failed"
                );
            }
            Err(e.into())
        }
    }
}

impl<M: Mode, P: std::fmt::Debug, DB: Database> std::fmt::Debug for Store<M, P, DB> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("mode", &std::any::type_name::<M>())
            .field("principal", &self.principal)
            .field("settings", &self.settings)
            .field("db", &self.db)
            .finish()
    }
}

impl<S, M, P, DB> FromRequestParts<S> for Store<M, P, DB>
where
    S: Send + Sync,
    M: Mode,
    P: Principal<S>,
    DB: Dialect,
    Db<DB>: FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Response> {
        let principal = P::extract(parts, state).await?;
        Ok(Store::new(Db::from_ref(state), principal))
    }
}

impl<M: Mode, P: PrincipalDocs, DB: Dialect> OperationInput for Store<M, P, DB> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        P::describe(builder);
    }
}

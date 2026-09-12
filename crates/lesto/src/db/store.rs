//! `Store<M, P, DB>`: a principal, a database handle, and `read`/`write` that run a closure
//! inside a transaction after checking a requirement.

use std::marker::PhantomData;

use crate::axum::extract::{FromRef, FromRequestParts};
use crate::axum::response::Response;
use crate::http::request::Parts;
use crate::{OperationBuilder, OperationInput};
use sqlx::{Database, Transaction};

use crate::db::dialect::Dialect;
use crate::db::error::Error;
use crate::db::handle::Db;
use crate::db::mode::{Mode, Writable};
use crate::db::principal::{Principal, PrincipalDocs};
use crate::db::requirement::Requirement;

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
    _mode: PhantomData<fn() -> M>,
}

impl<M: Mode, P, DB: Dialect> Store<M, P, DB> {
    /// Build a store by hand (tests, background jobs). Handlers get theirs by extraction.
    pub fn new(db: Db<DB>, principal: P) -> Self {
        Self {
            db,
            principal,
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
    pub async fn read<T, E, R, F>(&self, requirement: R, f: F) -> Result<T, Error>
    where
        R: Requirement<P>,
        E: Into<Error>,
        F: for<'c> AsyncFnOnce(&'c mut DB::Connection) -> Result<T, E>,
    {
        requirement.check(&self.principal)?;
        let tx = self.db.reads().begin_with(DB::BEGIN_READ_ONLY).await?;
        run(tx, f).await
    }

    /// Check `requirement`, open a read-write transaction on the primary, run `f`, commit.
    /// Any error rolls back. Only available when `M: Writable` (`ReadWrite`).
    pub async fn write<T, E, R, F>(&self, requirement: R, f: F) -> Result<T, Error>
    where
        M: Writable,
        R: Requirement<P>,
        E: Into<Error>,
        F: for<'c> AsyncFnOnce(&'c mut DB::Connection) -> Result<T, E>,
    {
        requirement.check(&self.principal)?;
        let tx = self.db.primary().begin().await?;
        run(tx, f).await
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

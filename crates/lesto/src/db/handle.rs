//! The database handle stored in the application state.

use sqlx::{Database, Pool};

/// A primary pool plus an optional read replica.
///
/// Put it in your state and implement `FromRef<AppState> for Db<YourDb>` (or derive it with
/// axum's `FromRef`): stores fetch it from there.
pub struct Db<DB: Database> {
    primary: Pool<DB>,
    replica: Option<Pool<DB>>,
}

impl<DB: Database> Db<DB> {
    pub fn new(primary: Pool<DB>) -> Self {
        Self {
            primary,
            replica: None,
        }
    }

    /// Route read-only transactions to `replica`.
    pub fn with_replica(mut self, replica: Pool<DB>) -> Self {
        self.replica = Some(replica);
        self
    }

    /// The pool used by `write`.
    pub fn primary(&self) -> &Pool<DB> {
        &self.primary
    }

    /// The pool used by `read`: the replica when configured, else the primary.
    pub fn reads(&self) -> &Pool<DB> {
        self.replica.as_ref().unwrap_or(&self.primary)
    }
}

impl<DB: Database> Clone for Db<DB> {
    fn clone(&self) -> Self {
        Self {
            primary: self.primary.clone(),
            replica: self.replica.clone(),
        }
    }
}

impl<DB: Database> std::fmt::Debug for Db<DB> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("replica", &self.replica.is_some())
            .finish_non_exhaustive()
    }
}

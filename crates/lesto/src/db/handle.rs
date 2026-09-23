//! The database handle stored in the application state.

use sqlx::{Database, Pool};

/// A primary pool plus an optional read replica.
///
/// Put it in your state and implement `FromRef<AppState> for Db<YourDb>` (or derive it with
/// axum's `FromRef`): stores fetch it from there.
pub struct Db<DB: Database> {
    primary: Pool<DB>,
    replica: Option<Pool<DB>>,
    conflict_retries: u8,
}

impl<DB: Database> Db<DB> {
    /// A handle on `primary`, with no read replica and the default conflict retry budget.
    pub fn new(primary: Pool<DB>) -> Self {
        Self {
            primary,
            replica: None,
            conflict_retries: Self::DEFAULT_CONFLICT_RETRIES,
        }
    }

    /// How many times `read_with` / `write_with` re-run their closure after a transient
    /// conflict, before answering 409.
    pub const DEFAULT_CONFLICT_RETRIES: u8 = 2;

    /// Route read-only transactions to `replica`.
    pub fn with_replica(mut self, replica: Pool<DB>) -> Self {
        self.replica = Some(replica);
        self
    }

    /// How many extra attempts a conflicting transaction gets. `0` disables retrying.
    ///
    /// Policy, not a per-call decision, which is why it lives here and not in a fourth argument
    /// to every store method. Only `read_with` / `write_with` retry: `read` and `write` run at
    /// the database's own isolation, where there is no conflict to retry.
    pub fn with_conflict_retries(mut self, retries: u8) -> Self {
        self.conflict_retries = retries;
        self
    }

    /// The configured retry budget.
    pub fn conflict_retries(&self) -> u8 {
        self.conflict_retries
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
            conflict_retries: self.conflict_retries,
        }
    }
}

impl<DB: Database> std::fmt::Debug for Db<DB> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("replica", &self.replica.is_some())
            .field("conflict_retries", &self.conflict_retries)
            .finish_non_exhaustive()
    }
}

//! What a store method needs the database to guarantee while its transaction runs.

/// The guarantee a transaction asks for, passed to
/// [`Store::read_with`](crate::db::Store::read_with) and
/// [`Store::write_with`](crate::db::Store::write_with).
///
/// A value, not a type parameter: it changes what the database does, not which methods compile.
///
/// Every variant renders to something every supported database can do, so none of them can be
/// refused — what changes per database is *how*:
///
/// | | Postgres | MySQL | SQLite |
/// |---|---|---|---|
/// | [`Default`](Self::Default) | `BEGIN` (read committed) | `START TRANSACTION` (repeatable read) | `BEGIN` |
/// | [`Snapshot`](Self::Snapshot) | `ISOLATION LEVEL REPEATABLE READ` | `ISOLATION LEVEL REPEATABLE READ` | `BEGIN DEFERRED` |
/// | [`Serializable`](Self::Serializable) | `ISOLATION LEVEL SERIALIZABLE` | `ISOLATION LEVEL SERIALIZABLE` | `BEGIN IMMEDIATE` when writing |
///
/// SQLite has no isolation levels because it only ever provides one: a single writer at a time,
/// which is serializable already. There the question is *when* the write lock is taken, and
/// `Serializable` takes it up front so a check-then-act does not meet `SQLITE_BUSY` halfway
/// through.
///
/// # Conflicts
///
/// `Snapshot` and `Serializable` can make the database refuse a transaction that would have
/// broken the guarantee, rather than making it wait. lesto answers that with **409** and
/// `Retry-After: 0` (see [`Error`](crate::db::Error)); the request is safe to send again. Under
/// [`Default`](Self::Default) that cannot happen, which is why it stays the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Isolation {
    /// Whatever the database does on its own. No conflict errors to handle.
    #[default]
    Default,

    /// One stable snapshot for the whole transaction: two identical queries in the same
    /// transaction see the same rows, and no rows appear that were committed after it began.
    ///
    /// Enough to read a consistent picture across several queries. Not enough for
    /// check-then-act: two transactions can each check and each write, because neither sees the
    /// other. That needs [`Serializable`](Self::Serializable).
    Snapshot,

    /// The transaction behaves as if it ran alone: a check made inside it still holds when it
    /// commits.
    ///
    /// This is what a check-then-act needs — read a row to decide, then write based on that
    /// decision. The alternative, and often the better one, is a unique constraint, which
    /// protects the table against every writer rather than only against the ones that go
    /// through this method.
    Serializable,
}

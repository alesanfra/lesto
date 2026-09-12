//! Store modes: `ReadOnly` and `ReadWrite`, checked at compile time.

mod sealed {
    pub trait Sealed {}
}

/// A store mode. Implemented by [`ReadOnly`] and [`ReadWrite`] only.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a store mode",
    label = "expected `lesto::db::ReadOnly` or `lesto::db::ReadWrite`",
    note = "a store is declared as `YourStore<ReadOnly, Principal>` or `YourStore<ReadWrite, Principal>`"
)]
pub trait Mode: sealed::Sealed + Send + Sync + 'static {
    /// `true` for [`ReadOnly`]: transactions are opened read-only and, when configured, on the
    /// read replica.
    const READ_ONLY: bool;
}

/// A mode that allows `write`. Implemented by [`ReadWrite`] only.
#[diagnostic::on_unimplemented(
    message = "this store is read-only: `{Self}` does not allow `write`",
    label = "`write` needs a `ReadWrite` store",
    note = "declare the handler argument as `YourStore<ReadWrite, _>`, or bound the impl block with `M: lesto::db::Writable` so the method is only available on read-write stores"
)]
pub trait Writable: Mode {}

/// Read-only store: methods may only call `read`; transactions are read-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadOnly;

/// Read-write store: methods may call `read` and `write`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadWrite;

impl sealed::Sealed for ReadOnly {}
impl sealed::Sealed for ReadWrite {}

impl Mode for ReadOnly {
    const READ_ONLY: bool = true;
}

impl Mode for ReadWrite {
    const READ_ONLY: bool = false;
}

impl Writable for ReadWrite {}

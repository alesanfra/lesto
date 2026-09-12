//! sqlx stores: one transaction per method, principals and permissions from the type
//! signature. Enabled by the `db` feature plus one of `postgres`, `mysql`, `sqlite`.
//!
//! A **store** is a handler argument that bundles a database handle with the **principal**
//! (who is calling) and runs each of its methods in its own transaction:
//!
//! ```ignore
//! #[derive(lesto::db::Store)]
//! pub struct NoteStore<M, P>(lesto::db::Store<M, P, Sqlite>);
//!
//! impl<M: Mode, P> NoteStore<M, P> {
//!     pub async fn list(&self) -> Result<Vec<Note>, Error> {
//!         self.read(Anyone, async |conn| {
//!             sqlx::query_as("SELECT id, author, text FROM notes").fetch_all(conn).await
//!         }).await
//!     }
//! }
//!
//! impl<M: Writable> NoteStore<M, User> {
//!     pub async fn create(&self, body: NoteCreate) -> Result<Note, Error> {
//!         let author = self.principal().id;
//!         self.write("notes:write", async |conn| { /* INSERT .. RETURNING */ }).await
//!     }
//! }
//!
//! #[crate::get("/notes")]
//! async fn list_notes(store: NoteStore<ReadOnly, Public>) -> Result<Json<Vec<Note>>, Error> {
//!     Ok(Json(store.list().await?))
//! }
//! ```
//!
//! - `ReadOnly` / `ReadWrite` decide which methods compile and how transactions are opened.
//! - `Public` is the anonymous principal; implement [`Authenticated`] for your user type.
//! - `read` / `write` take a [`Requirement`] first: [`Anyone`] or a permission string, checked
//!   before the transaction opens. A permission on a `Public` store does not compile.
//! - [`Error`] answers as RFC 9457 and documents 403/404/409 in OpenAPI.
//!

pub mod dialect;
pub mod error;
pub mod handle;
pub mod mode;
pub mod principal;
pub mod requirement;
pub mod store;

pub use dialect::Dialect;
pub use error::{Error, ResultExt};
pub use handle::Db;
pub use mode::{Mode, ReadOnly, ReadWrite, Writable};
pub use principal::{Authenticated, Principal, PrincipalDocs, Public};
pub use requirement::{Anyone, Requirement};
pub use store::Store;

/// Derive for the newtype around [`struct@Store`]: forwards extraction and documentation, adds `Deref`.
pub use lesto_macros::Store;

pub use sqlx;

/// Everything a store module needs.
pub mod prelude {
    pub use crate::db::{
        Anyone, Authenticated, Db, Error, Mode, Principal, Public, ReadOnly, ReadWrite, ResultExt,
        Store, Writable,
    };
}

//! The notes resource: model, store, handlers.

pub mod handlers;
pub mod model;
pub mod store;

pub use handlers::routes;
pub use model::{Note, NoteCreate, NoteUpdate};
pub use store::NoteStore;

//! HTTP handlers. The store in the signature says who may call and whether it writes.

use lesto::RouteSet;
use lesto::db::{Error, Public, ReadOnly, ReadWrite};
use lesto::prelude::*;

use super::model::{Note, NoteCreate, NoteUpdate};
use super::store::NoteStore;
use crate::auth::User;
use crate::state::AppState;

/// List notes.
#[lesto::get("/notes", tag = "notes")]
async fn list(store: NoteStore<ReadOnly, Public>) -> Result<Json<Vec<Note>>, Error> {
    Ok(Json(store.list().await?))
}

/// Read one note.
#[lesto::get("/notes/{id}", tag = "notes")]
async fn get(store: NoteStore<ReadOnly, Public>, Path(id): Path<i64>) -> Result<Json<Note>, Error> {
    Ok(Json(store.get(id).await?))
}

/// Create a note (needs `notes:write`).
#[lesto::post("/notes", status = 201, tag = "notes")]
async fn create(
    store: NoteStore<ReadWrite, User>,
    Json(body): Json<NoteCreate>,
) -> Result<Json<Note>, Error> {
    Ok(Json(store.create(body).await?))
}

/// Edit a note (needs `notes:write`, author only).
#[lesto::patch("/notes/{id}", tag = "notes")]
async fn update(
    store: NoteStore<ReadWrite, User>,
    Path(id): Path<i64>,
    Json(body): Json<NoteUpdate>,
) -> Result<Json<Note>, Error> {
    Ok(Json(store.update(id, body).await?))
}

/// Delete a note (needs `notes:delete`).
#[lesto::delete("/notes/{id}", status = 204, tag = "notes")]
async fn delete(store: NoteStore<ReadWrite, User>, Path(id): Path<i64>) -> Result<(), Error> {
    store.delete(id).await
}

pub fn routes() -> RouteSet<AppState> {
    routes![list, get, create, update, delete]
}

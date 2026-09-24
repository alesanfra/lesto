//! HTTP handlers. The store in the signature says who may call and whether it writes.
//!
//! Every route but `delete` is also served over MCP (tutorial chapter 16): the JSON routes as
//! tools, so agents may read, create and edit notes; a note's text as a resource an agent can
//! attach as context; a prompt to rewrite a note. Deleting stays a decision for a human with an
//! HTTP client.

use lesto::RouteSet;
use lesto::db::{Error, Public, ReadOnly, ReadWrite};
use lesto::mcp::Prompt;
use lesto::prelude::*;

use super::model::{Note, NoteCreate, NoteUpdate};
use super::store::NoteStore;
use crate::auth::User;
use crate::state::AppState;

/// List notes.
#[lesto::get("/notes", tag = "notes", mcp(tool, name = "list_notes"))]
async fn list(store: NoteStore<ReadOnly, Public>) -> Result<Json<Vec<Note>>, Error> {
    Ok(Json(store.list().await?))
}

/// Read one note.
#[lesto::get("/notes/{id}", tag = "notes", mcp(tool, name = "get_note"))]
async fn get(store: NoteStore<ReadOnly, Public>, Path(id): Path<i64>) -> Result<Json<Note>, Error> {
    Ok(Json(store.get(id).await?))
}

/// Create a note (needs `notes:write`).
#[lesto::post("/notes", status = 201, tag = "notes", mcp(tool, name = "create_note"))]
async fn create(
    store: NoteStore<ReadWrite, User>,
    Json(body): Json<NoteCreate>,
) -> Result<Json<Note>, Error> {
    Ok(Json(store.create(body).await?))
}

/// Edit a note (needs `notes:write`, author only).
#[lesto::patch("/notes/{id}", tag = "notes", mcp(tool, name = "update_note"))]
async fn update(
    store: NoteStore<ReadWrite, User>,
    Path(id): Path<i64>,
    Json(body): Json<NoteUpdate>,
) -> Result<Json<Note>, Error> {
    Ok(Json(store.update(id, body).await?))
}

/// A note's text.
#[lesto::get("/notes/{id}/text", tag = "notes", mcp(resource, name = "note_text"))]
async fn text(store: NoteStore<ReadOnly, Public>, Path(id): Path<i64>) -> Result<String, Error> {
    Ok(store.get(id).await?.text)
}

#[lesto::model]
struct Tidy {
    /// Who will read the note, e.g. `the team`.
    #[garde(length(min = 1))]
    audience: Option<String>,
}

/// Rewrite a note more clearly.
#[lesto::get("/prompts/tidy/{id}", tag = "prompts", mcp(prompt, name = "tidy_note"))]
async fn tidy(
    store: NoteStore<ReadOnly, Public>,
    Path(id): Path<i64>,
    Query(tidy): Query<Tidy>,
) -> Result<Prompt, Error> {
    let note = store.get(id).await?;
    let audience = tidy.audience.unwrap_or_else(|| "its author".into());
    Ok(Prompt::new().user(format!(
        "Rewrite this note so that {audience} understands it at a glance. Keep every fact.\n\n{}",
        note.text
    )))
}

/// Delete a note (needs `notes:delete`).
#[lesto::delete("/notes/{id}", status = 204, tag = "notes")]
async fn delete(store: NoteStore<ReadWrite, User>, Path(id): Path<i64>) -> Result<(), Error> {
    store.delete(id).await
}

pub fn routes() -> RouteSet<AppState> {
    routes![list, get, create, update, text, tidy, delete]
}

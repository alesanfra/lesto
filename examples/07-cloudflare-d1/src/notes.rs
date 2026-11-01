//! The notes API: three routes over a D1 table.

use axum::Extension;
use lesto::prelude::*;
use worker::{Env, send::SendWrapper, wasm_bindgen::JsValue};

/// A note. The same type is the D1 row and the JSON body.
#[lesto::model(views(Create(text)))]
#[derive(Debug, Clone)]
pub struct Note {
    #[garde(skip)]
    pub id: i64,
    /// The note itself.
    #[garde(length(min = 1, max = 280))]
    pub text: String,
}

/// D1 errors are the server's problem: the detail is logged, the client gets a plain 500.
fn internal(error: worker::Error) -> HttpError {
    HttpError::internal(format!("database error: {error}"))
}

fn database(env: &Env) -> Result<worker::D1Database, HttpError> {
    env.d1("DB").map_err(internal)
}

/// List the notes, newest first.
#[lesto::get("/notes")]
#[worker::send]
async fn list(Extension(env): Extension<SendWrapper<Env>>) -> Result<Json<Vec<Note>>, HttpError> {
    let rows = database(&env)?
        .prepare("SELECT id, text FROM notes ORDER BY id DESC")
        .all()
        .await
        .map_err(internal)?;
    Ok(Json(rows.results::<Note>().map_err(internal)?))
}

/// One note by id.
#[lesto::get("/notes/{id}", responses(404))]
#[worker::send]
async fn get(
    Extension(env): Extension<SendWrapper<Env>>,
    Path(id): Path<i64>,
) -> Result<Json<Note>, HttpError> {
    let note = database(&env)?
        .prepare("SELECT id, text FROM notes WHERE id = ?1")
        .bind(&[JsValue::from_f64(id as f64)])
        .map_err(internal)?
        .first::<Note>(None)
        .await
        .map_err(internal)?;
    note.map(Json)
        .ok_or_else(|| HttpError::not_found(format!("note {id} not found")))
}

/// Add a note.
#[lesto::post("/notes", status = 201)]
#[worker::send]
async fn create(
    Extension(env): Extension<SendWrapper<Env>>,
    Json(body): Json<NoteCreate>,
) -> Result<Json<Note>, HttpError> {
    let note = database(&env)?
        .prepare("INSERT INTO notes (text) VALUES (?1) RETURNING id, text")
        .bind(&[JsValue::from_str(&body.text)])
        .map_err(internal)?
        .first::<Note>(None)
        .await
        .map_err(internal)?;
    note.map(Json)
        .ok_or_else(|| HttpError::internal("the insert returned no row"))
}

pub fn routes() -> lesto::RouteSet<()> {
    routes![list, get, create]
}

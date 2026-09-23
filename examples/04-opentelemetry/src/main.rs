//! A small, unauthenticated API whose traces and logs end up in a local Jaeger or OpenObserve.
//!
//! The interesting part is what is *not* here: no telemetry code. `lesto` is built with the
//! `otel` feature, the environment says where the collector is, and `App::serve` does the rest
//! — console logs, OTLP export, a flush on shutdown. See `README.md` next to this file.

use lesto::axum::extract::FromRef;
use lesto::db::prelude::*;
use lesto::prelude::*;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};

// ---- state ------------------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    db: Db<Sqlite>,
}

impl FromRef<AppState> for Db<Sqlite> {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}

// ---- the store --------------------------------------------------------------------------------

#[lesto::model(views(Create(text)))]
#[derive(sqlx::FromRow)]
struct Note {
    #[garde(skip)]
    id: i64,

    #[garde(length(min = 1, max = 280))]
    text: String,
}

/// Anonymous notes: the principal is `Public`, so every requirement is `Anyone`.
#[derive(lesto::db::Store)]
struct NoteStore<M, P>(Store<M, P, Sqlite>);

impl<M: Mode, P> NoteStore<M, P> {
    async fn list(&self) -> Result<Vec<Note>, Error> {
        self.read(Anyone, async |conn| {
            sqlx::query_as::<_, Note>("SELECT id, text FROM notes ORDER BY id")
                .fetch_all(conn)
                .await
        })
        .await
    }
}

impl<M: Writable, P> NoteStore<M, P> {
    async fn create(&self, body: NoteCreate) -> Result<Note, Error> {
        self.write(Anyone, async |conn| {
            sqlx::query_as::<_, Note>("INSERT INTO notes (text) VALUES (?) RETURNING id, text")
                .bind(&body.text)
                .fetch_one(conn)
                .await
        })
        .await
    }
}

// ---- handlers ---------------------------------------------------------------------------------

/// Say hello. One span, no database.
#[lesto::get("/hello")]
async fn hello() -> &'static str {
    "Hello, lesto!"
}

/// List the notes. The request span gets a child span for the transaction.
#[lesto::get("/notes", tag = "notes", state = AppState)]
async fn list_notes(store: NoteStore<ReadOnly, Public>) -> Result<Json<Vec<Note>>, Error> {
    Ok(Json(store.list().await?))
}

/// Create a note.
#[lesto::post("/notes", status = 201, tag = "notes", state = AppState)]
async fn create_note(
    store: NoteStore<ReadWrite, Public>,
    Json(body): Json<NoteCreate>,
) -> Result<Json<Note>, Error> {
    lesto::tracing::info!(length = body.text.len(), "creating a note");
    Ok(Json(store.create(body).await?))
}

/// Fail on purpose: a `500` marks the span as an error in the trace view.
#[lesto::get("/boom", responses(500))]
async fn boom() -> Result<&'static str, HttpError> {
    Err(HttpError::internal("this route always fails"))
}

/// Query a table that does not exist: the failure is recorded on the store span.
#[lesto::get("/broken", tag = "notes", state = AppState, responses(500))]
async fn broken(store: NoteStore<ReadOnly, Public>) -> Result<Json<i64>, Error> {
    let count = store
        .read(Anyone, async |conn| {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nowhere")
                .fetch_one(conn)
                .await
        })
        .await?;
    Ok(Json(count))
}

fn build_app() -> App<AppState> {
    App::new()
        .title("OpenTelemetry demo")
        .description("Traces and logs leave through the `OTEL_*` variables alone.")
        .tag("notes", "Anonymous notes")
        .routes(routes![hello, list_notes, create_note, boom, broken])
}

#[lesto::main]
async fn main() -> std::io::Result<()> {
    // One connection: every `sqlite::memory:` connection is a database of its own.
    let pool: Pool<Sqlite> = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory SQLite");
    sqlx::query("CREATE TABLE notes (id INTEGER PRIMARY KEY, text TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("schema");

    build_app()
        .with_state(AppState { db: Db::new(pool) })
        .serve()
        .await
}

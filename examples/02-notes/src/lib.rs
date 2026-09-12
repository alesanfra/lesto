//! The notes application: state, authentication, and the `notes` module (model, store,
//! handlers). `build_app` assembles the routes; `connect` opens the database.

pub mod auth;
pub mod notes;
pub mod state;

pub use auth::{Tokens, User};
pub use state::AppState;

use lesto::App;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};

pub fn build_app() -> App<AppState> {
    App::<AppState>::new()
        .title("Notes")
        .version("0.1.0")
        .description("Public reads, authenticated writes, permission-checked deletes")
        .routes(notes::routes())
}

/// An in-memory SQLite database with the schema applied.
///
/// `sqlite::memory:` is per connection: with one connection the pool is one database. A real
/// deployment would connect to a file or a server and run migrations.
pub async fn connect() -> Pool<Sqlite> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open sqlite");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS notes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            author TEXT NOT NULL,
            text TEXT NOT NULL UNIQUE
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");
    pool
}

//! The notes application: state, authentication, and the `notes` module (model, store,
//! handlers). `build_app` assembles the routes; `connect` opens the database.

pub mod auth;
pub mod notes;
pub mod state;

pub use auth::{Tokens, User};
pub use state::AppState;

use lesto::App;
use lesto::mcp::Mcp;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};

pub fn build_app() -> App<AppState> {
    App::<AppState>::new()
        .title("Notes")
        .version("0.1.0")
        .description("Public reads, authenticated writes, permission-checked deletes")
        .routes(notes::routes())
        // The routes marked `mcp(tool, ..)` are served to agents at /mcp.
        .mcp(Mcp::new().instructions(
            "Short notes. Anyone can read; writing needs a bearer token with notes:write.",
        ))
}

/// An in-memory SQLite database with the migrations in `migrations/` applied.
///
/// `sqlite::memory:` is per connection: with one connection the pool is one database. A real
/// deployment connects to a file or a server, with a pool sized for it (chapter 13).
pub async fn connect() -> Pool<Sqlite> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        // A request waits this long for a free connection, then answers 500.
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect("sqlite::memory:")
        .await
        .expect("open sqlite");
    // Embedded at compile time from `migrations/`; applied ones are skipped.
    sqlx::migrate!().run(&pool).await.expect("migrate");
    pool
}

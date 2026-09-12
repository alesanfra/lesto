//! Application state: the database handle and the token table.

use lesto::axum::extract::FromRef;
use lesto::db::Db;
use sqlx::Sqlite;

use crate::auth::Tokens;

#[derive(Clone)]
pub struct AppState {
    pub db: Db<Sqlite>,
    pub tokens: Tokens,
}

/// Stores fetch the database handle from the state through `FromRef`.
impl FromRef<AppState> for Db<Sqlite> {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}

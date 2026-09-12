// Chapter 10: a route module
use lesto::prelude::*;

use super::models::{UserIn, UserOut};
use crate::state::{AppState, DbPool};

/// List users.
#[lesto::get("/", tag = "users")]
pub async fn list(State(db): State<DbPool>) -> Json<Vec<UserOut>> {
    Json(db.users.lock().unwrap().clone())
}

/// Create a user.
#[lesto::post("/", status = 201, tag = "users", responses(409))]
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<UserIn>,
) -> Result<Json<UserOut>, HttpError> {
    let mut users = state.db.users.lock().unwrap();
    if users.iter().any(|u| u.name == body.name) {
        return Err(HttpError::conflict("name already taken"));
    }
    let user = UserOut {
        id: users.len() as u64 + 1,
        name: body.name,
    };
    users.push(user.clone());
    Ok(Json(user))
}

/// One user by id.
#[lesto::get("/{id}", tag = "users", responses(404))]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<UserOut>, HttpError> {
    state
        .db
        .users
        .lock()
        .unwrap()
        .iter()
        .find(|u| u.id == id)
        .cloned()
        .map(Json)
        .ok_or(crate::errors::RepoError::NotFound)
        .map_err(Into::into)
}

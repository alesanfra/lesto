mod handlers;
pub mod models;

use lesto::{RouteSet, routes};

use crate::state::AppState;

pub fn routes() -> RouteSet<AppState> {
    routes![handlers::list, handlers::create, handlers::get]
}

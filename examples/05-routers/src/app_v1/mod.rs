//! `/api/app/v1`: the shop API. One sub-router per resource, each with its own prefix.

mod orders;
mod products;

use lesto::App;

use crate::AppState;

pub fn router() -> App<AppState> {
    App::<AppState>::new()
        .tag("products", "The catalog")
        .tag("orders", "Placing orders")
        .nest("/products", products::router())
        .nest("/orders", orders::router())
}

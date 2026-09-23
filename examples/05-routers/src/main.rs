//! Two APIs in one service, mounted like FastAPI routers:
//!
//! ```sh
//! LESTO_PORT=8765 cargo run -p routers      # or: lesto dev -p routers --port 8765
//! curl -X POST http://127.0.0.1:8765/api/app/v1/products \
//!      -H 'content-type: application/json' -d '{"name": "Espresso", "price_cents": 120}'
//! curl -X POST http://127.0.0.1:8765/api/app/v1/orders \
//!      -H 'content-type: application/json' -d '{"product_id": 1, "quantity": 3}'
//! curl http://127.0.0.1:8765/api/analytics/v1/sales
//! open http://127.0.0.1:8765/docs
//! ```

use routers::{AppState, build_app};

#[lesto::main]
async fn main() -> std::io::Result<()> {
    build_app().with_state(AppState::default()).serve().await
}

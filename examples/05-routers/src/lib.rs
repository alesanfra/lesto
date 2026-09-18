//! A shop with two APIs over the same data:
//!
//! - `/api/app/v1`: what the shop front end calls (products, orders);
//! - `/api/analytics/v1`: what the back office dashboards call (sales figures).
//!
//! Each API is its own module exposing `router() -> App<AppState>`, the equivalent of a FastAPI
//! `APIRouter`; `build_app` mounts them with `nest`, the equivalent of `include_router`.
//!
//! ```text
//! build_app()                          FastAPI
//! ├── nest("/api/app/v1", ..)          app.include_router(app_v1.router, prefix="/api/app/v1")
//! │   ├── nest("/products", ..)          router.include_router(products.router, prefix="/products")
//! │   └── nest("/orders", ..)            router.include_router(orders.router, prefix="/orders")
//! └── nest("/api/analytics/v1", ..)    app.include_router(analytics_v1.router, prefix="/api/analytics/v1")
//! ```

pub mod analytics_v1;
pub mod app_v1;
pub mod state;

pub use state::AppState;

use lesto::App;

/// The whole service: both APIs, one OpenAPI document, docs at `/docs`.
pub fn build_app() -> App<AppState> {
    App::<AppState>::new()
        .title("Shop")
        .version("1.0.0")
        .description("The shop API and the analytics API, served by one process")
        .nest("/api/app/v1", app_v1::router())
        .nest("/api/analytics/v1", analytics_v1::router())
}

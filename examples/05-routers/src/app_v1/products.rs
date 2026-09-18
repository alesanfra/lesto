//! `/api/app/v1/products`. Paths here are relative to the prefix the router is mounted at.

use lesto::prelude::*;

use crate::AppState;
use crate::state::Product;

#[derive(Deserialize, JsonSchema, Validate)]
pub struct ProductCreate {
    #[garde(length(min = 1, max = 100))]
    pub name: String,
    /// Unit price, in cents.
    #[garde(range(min = 1))]
    pub price_cents: u64,
}

/// List the catalog.
#[lesto::get("/", tag = "products")]
pub async fn list(State(state): State<AppState>) -> Json<Vec<Product>> {
    Json(state.db.lock().unwrap().products.clone())
}

/// One product by id.
#[lesto::get("/{id}", tag = "products", responses(404))]
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<Product>, HttpError> {
    let db = state.db.lock().unwrap();
    db.products
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .map(Json)
        .ok_or_else(|| HttpError::not_found(format!("product {id} not found")))
}

/// Add a product to the catalog.
#[lesto::post("/", status = 201, tag = "products")]
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<ProductCreate>,
) -> Json<Product> {
    let mut db = state.db.lock().unwrap();
    let product = Product {
        id: db.products.len() as u64 + 1,
        name: body.name,
        price_cents: body.price_cents,
    };
    db.products.push(product.clone());
    Json(product)
}

pub fn router() -> App<AppState> {
    App::<AppState>::new().routes(routes![list, get, create])
}

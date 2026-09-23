//! `/api/app/v1/orders`.

use lesto::prelude::*;

use crate::AppState;
use crate::state::Order;

#[lesto::model]
pub struct OrderCreate {
    #[garde(skip)]
    pub product_id: u64,
    #[garde(range(min = 1, max = 100))]
    pub quantity: u32,
}

/// Place an order for one product.
#[lesto::post("/", status = 201, tag = "orders", responses(422))]
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<OrderCreate>,
) -> Result<Json<Order>, HttpError> {
    let mut db = state.db.lock().unwrap();
    let Some(product) = db.products.iter().find(|p| p.id == body.product_id) else {
        return Err(HttpError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("product {} does not exist", body.product_id),
        ));
    };
    let order = Order {
        id: db.orders.len() as u64 + 1,
        product_id: product.id,
        quantity: body.quantity,
        total_cents: product.price_cents * u64::from(body.quantity),
    };
    db.orders.push(order.clone());
    Ok(Json(order))
}

pub fn router() -> App<AppState> {
    App::<AppState>::new().routes(routes![create])
}

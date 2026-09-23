//! `/api/analytics/v1`: read-only figures over the orders the shop API recorded.

use std::collections::HashMap;

use lesto::prelude::*;

use crate::AppState;

#[lesto::model]
pub struct SalesSummary {
    pub orders: usize,
    pub items_sold: u64,
    pub revenue_cents: u64,
}

#[lesto::model]
pub struct ProductSales {
    pub product_id: u64,
    pub name: String,
    pub items_sold: u64,
    pub revenue_cents: u64,
}

#[lesto::model]
pub struct TopQuery {
    /// How many products to return.
    #[garde(range(min = 1, max = 50))]
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    5
}

/// Totals over every order.
#[lesto::get("/sales", tag = "analytics")]
pub async fn sales(State(state): State<AppState>) -> Json<SalesSummary> {
    let db = state.db.lock().unwrap();
    Json(SalesSummary {
        orders: db.orders.len(),
        items_sold: db.orders.iter().map(|o| u64::from(o.quantity)).sum(),
        revenue_cents: db.orders.iter().map(|o| o.total_cents).sum(),
    })
}

/// Best-selling products, by revenue.
#[lesto::get("/products/top", tag = "analytics")]
pub async fn top_products(
    State(state): State<AppState>,
    Query(q): Query<TopQuery>,
) -> Json<Vec<ProductSales>> {
    let db = state.db.lock().unwrap();
    let mut by_product: HashMap<u64, (u64, u64)> = HashMap::new();
    for order in &db.orders {
        let entry = by_product.entry(order.product_id).or_default();
        entry.0 += u64::from(order.quantity);
        entry.1 += order.total_cents;
    }
    let mut top: Vec<ProductSales> = db
        .products
        .iter()
        .filter_map(|p| {
            let &(items_sold, revenue_cents) = by_product.get(&p.id)?;
            Some(ProductSales {
                product_id: p.id,
                name: p.name.clone(),
                items_sold,
                revenue_cents,
            })
        })
        .collect();
    top.sort_by_key(|p| std::cmp::Reverse(p.revenue_cents));
    top.truncate(q.limit);
    Json(top)
}

pub fn router() -> App<AppState> {
    App::<AppState>::new()
        .tag("analytics", "Sales figures for the back office")
        .routes(routes![sales, top_products])
}

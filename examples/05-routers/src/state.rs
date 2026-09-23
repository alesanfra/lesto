//! The state both APIs share: the app API writes it, the analytics API reads it.

use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct AppState {
    pub db: Arc<Mutex<Data>>,
}

/// An in-memory stand-in for a database.
#[derive(Default)]
pub struct Data {
    pub products: Vec<Product>,
    pub orders: Vec<Order>,
}

#[lesto::model]
#[derive(Clone)]
pub struct Product {
    pub id: u64,
    pub name: String,
    /// Unit price, in cents.
    pub price_cents: u64,
}

#[lesto::model]
#[derive(Clone)]
pub struct Order {
    pub id: u64,
    pub product_id: u64,
    pub quantity: u32,
    /// `quantity` × the product price at the time of the order, in cents.
    pub total_cents: u64,
}

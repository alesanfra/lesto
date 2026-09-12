use std::sync::{Arc, Mutex};

use lesto::axum::extract::FromRef;

#[derive(Clone, Default)]
pub struct DbPool {
    pub users: Arc<Mutex<Vec<crate::users::models::UserOut>>>,
}

#[derive(Clone, Default)]
pub struct AppState {
    pub db: DbPool,
    pub counter: Arc<Mutex<u64>>,
}

impl AppState {
    pub fn for_tests() -> Self {
        Self::default()
    }
}

// Chapter 8: sub-states with FromRef
impl FromRef<AppState> for DbPool {
    fn from_ref(s: &AppState) -> DbPool {
        s.db.clone()
    }
}

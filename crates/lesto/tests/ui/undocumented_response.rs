use lesto::axum::response::{IntoResponse, Response};
use lesto::prelude::*;

// A response type axum can send but lesto cannot document.
struct Csv(String);

impl IntoResponse for Csv {
    fn into_response(self) -> Response {
        self.0.into_response()
    }
}

#[lesto::get("/report")]
async fn report() -> Csv {
    Csv("a,b".into())
}

fn main() {
    let _app = App::<()>::new().routes(routes![report]);
}

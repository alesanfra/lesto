// `axum::Json<T>` with `T: Validate` documents fine and validates nothing: lesto warns (denied
// here so the case fails to compile and the message can be checked).
#![deny(deprecated)]

use lesto::prelude::*;

#[derive(Deserialize, JsonSchema, Validate)]
struct Signup {
    #[garde(length(min = 1))]
    name: String,
}

#[lesto::post("/signup")]
async fn signup(lesto::axum::Json(body): lesto::axum::Json<Signup>) -> String {
    body.name
}

fn main() {
    let _ = App::new().routes(routes![signup]);
}

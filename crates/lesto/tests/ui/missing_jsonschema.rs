use lesto::prelude::*;

// Response types need `JsonSchema` to be documented.
#[derive(Serialize)]
struct Output {
    name: String,
}

#[lesto::get("/items")]
async fn list() -> Json<Output> {
    Json(Output { name: "x".into() })
}

fn main() {
    let _app = App::<()>::new().routes(routes![list]);
}

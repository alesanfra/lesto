use lesto::prelude::*;

// `Json<T>` validates, so `T` must derive `garde::Validate`.
#[derive(Deserialize, JsonSchema)]
struct Input {
    name: String,
}

#[lesto::post("/items")]
async fn create(Json(input): Json<Input>) -> String {
    input.name
}

fn main() {
    let _app = App::<()>::new().routes(routes![create]);
}

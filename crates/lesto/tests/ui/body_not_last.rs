use lesto::prelude::*;

#[derive(Deserialize, JsonSchema, Validate)]
#[garde(allow_unvalidated)]
struct Input {
    name: String,
}

// The body extractor must be the last argument.
#[lesto::post("/items/{id}")]
async fn create(Json(input): Json<Input>, Path(id): Path<u64>) -> String {
    format!("{id} {}", input.name)
}

fn main() {
    let _app = App::<()>::new().routes(routes![create]);
}

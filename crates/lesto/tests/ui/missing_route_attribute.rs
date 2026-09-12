use lesto::prelude::*;

// The route attribute is missing: `routes!` cannot find the route metadata.
async fn health() -> &'static str {
    "ok"
}

fn main() {
    let _app = App::<()>::new().routes(routes![health]);
}

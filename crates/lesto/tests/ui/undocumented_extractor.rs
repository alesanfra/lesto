use lesto::axum::extract::FromRequestParts;
use lesto::http::request::Parts;
use lesto::prelude::*;

// A custom extractor that works at runtime but does not say how to document itself.
struct RequestId(String);

impl<S: Send + Sync> FromRequestParts<S> for RequestId {
    type Rejection = HttpError;
    async fn from_request_parts(_: &mut Parts, _: &S) -> Result<Self, HttpError> {
        Ok(RequestId("id".into()))
    }
}

#[lesto::get("/")]
async fn handler(RequestId(id): RequestId) -> String {
    id
}

fn main() {
    let _app = App::<()>::new().routes(routes![handler]);
}

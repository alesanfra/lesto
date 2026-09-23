use lesto::oidc::Jwt;
use lesto::prelude::*;

// The claims type of a `Jwt` is read from the token's JSON: it needs `Deserialize`.
struct Claims {
    email: String,
}

#[lesto::get("/me")]
async fn me(token: Jwt<Claims>) -> String {
    token.claims().email.clone()
}

fn main() {
    let _app = App::<()>::new().routes(routes![me]);
}

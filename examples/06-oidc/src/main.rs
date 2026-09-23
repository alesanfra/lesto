//! Chapter 9 against a real provider: `docker compose up -d`, then
//! `LESTO_OIDC_DISCOVERY_URL=http://localhost:8089/default/.well-known/openid-configuration
//! LESTO_OIDC_AUDIENCES=api://notes LESTO_PORT=8765 cargo run -p oidc-example`. See README.md.

use lesto::oidc::{Jwt, Oidc};
use lesto::prelude::*;

/// Anyone may ask.
#[lesto::get("/health")]
async fn health() -> &'static str {
    "ok"
}

/// Who the token says is calling.
#[lesto::get("/me")]
async fn me(token: Jwt) -> String {
    format!(
        "{} ({})",
        token.subject().unwrap_or("?"),
        token.scopes().collect::<Vec<_>>().join(" ")
    )
}

/// Admins only: the whole nested app requires the `admin` scope.
#[lesto::get("/stats")]
async fn stats() -> &'static str {
    "42 notes"
}

fn app(auth: Oidc) -> App {
    let admin = App::new()
        .routes(routes![stats])
        .protect(auth.clone().scopes(["admin"]));
    App::new()
        .title("OpenID Connect example")
        .oidc(auth)
        .routes(routes![health, me])
        .nest("/admin", admin)
}

#[lesto::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(auth) = Oidc::from_env().await? else {
        return Err("set LESTO_OIDC_DISCOVERY_URL (see examples/06-oidc/README.md)".into());
    };
    app(auth).serve().await?;
    Ok(())
}

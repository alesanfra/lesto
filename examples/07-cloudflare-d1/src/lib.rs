//! lesto on Cloudflare Workers, with a D1 database.
//!
//! The Worker's `fetch` handler is the only entry point: it hands every request to the axum
//! router `App::into_router` builds. Nothing here binds a socket (there is none on Workers), so
//! `App::serve` is not used; lesto compiles for `wasm32-unknown-unknown` without its server half.

mod notes;

use std::sync::OnceLock;

use axum::Router;
use lesto::App;
use tower_service::Service;
use worker::{Context, Env, HttpRequest, event, send::SendWrapper};

/// The routes, the OpenAPI document and the docs pages. Built once per isolate.
fn router() -> &'static Router {
    static ROUTER: OnceLock<Router> = OnceLock::new();
    ROUTER.get_or_init(|| {
        App::<()>::new()
            .title("Notes on Cloudflare D1")
            .version("1.0.0")
            .routes(notes::routes())
            .into_router()
    })
}

#[event(fetch)]
async fn fetch(
    mut req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> worker::Result<axum::http::Response<axum::body::Body>> {
    console_error_panic_hook::set_once();
    // `Env` holds JavaScript objects, which are not `Send`; Workers run one thread, so wrapping
    // them is sound. Handlers read the binding from the request's extensions.
    req.extensions_mut().insert(SendWrapper(env));
    Ok(router().clone().call(req).await?)
}

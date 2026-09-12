//! # lesto
//!
//! A FastAPI-flavoured web framework on top of [`axum`]:
//!
//! * declare a route with `#[lesto::get("/users/{id}")]` on an `async fn`;
//! * request bodies and query strings are deserialized **and validated** with [`garde`];
//!   failures answer `422` as RFC 9457 `application/problem+json`, one entry per failed check;
//! * an OpenAPI 3.1 document is derived from the handler's argument and return types via
//!   [`schemars`], served at `/openapi.json`, with Scalar at `/docs` and Swagger UI at `/swagger`;
//! * authentication extractors ([`Bearer`], [`Basic`], [`ApiKey`]) register their security
//!   scheme and requirement in the document;
//! * optional features: `db` (+ `postgres`/`mysql`/`sqlite`) adds sqlx stores with principals
//!   and permissions ([`db`]); `lambda` runs the app on AWS Lambda ([`lambda`]); `anyhow`
//!   converts `anyhow::Error` into a `500`.
//!
//! ```no_run
//! use lesto::prelude::*;
//!
//! #[derive(Deserialize, JsonSchema, Validate)]
//! struct CreateUser {
//!     #[garde(length(min = 1, max = 64))]
//!     name: String,
//!     #[garde(email)]
//!     email: String,
//! }
//!
//! #[derive(Serialize, JsonSchema)]
//! struct User { id: u64, name: String, email: String }
//!
//! /// Create a user.
//! #[lesto::post("/users", status = 201, tag = "users")]
//! async fn create_user(Json(body): Json<CreateUser>) -> Json<User> {
//!     Json(User { id: 1, name: body.name, email: body.email })
//! }
//!
//! /// Fetch a user by id.
//! #[lesto::get("/users/{id}", tag = "users", responses(404))]
//! async fn get_user(Path(id): Path<u64>) -> Result<Json<User>, HttpError> {
//!     Err(HttpError::not_found(format!("user {id} not found")))
//! }
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     App::new()
//!         .title("Users API")
//!         .routes(routes![create_user, get_user])
//!         .serve()
//!         .await
//! }
//! ```

pub mod app;
#[cfg(feature = "db")]
pub mod db;
pub mod docs;
pub mod error;
pub mod extract;
#[cfg(feature = "lambda")]
pub mod lambda;
pub mod openapi;
pub mod operation;
pub mod route;
pub mod security;

pub use app::{App, bind_address, listener, shutdown_signal};
pub use docs::DocsAssets;
pub use error::{
    ErrorFormat, HttpError, HttpErrorBody, HttpValidationError, IntoStatus, Problem, ProblemError,
    Rejection, ValidationError, ValidationErrorItem,
};
pub use extract::{Json, Path, Query};
pub use operation::{OperationBuilder, OperationHandler, OperationInput, OperationOutput};
pub use route::{
    PendingOperation, RouteInfo, RouteMeta, RouteSet, delete, get, head, options, patch, post, put,
};
pub use security::{
    ApiKey, ApiKeyScheme, AuthScheme, Basic, BasicAuth, Bearer, BearerAuth, Security,
};

// The attribute macros share names with the `RouteMeta` constructors above. Both are useful:
// `#[lesto::get("/x")]` in attribute position and `lesto::get("/x")` in expression position.
// Rust resolves attributes in the macro namespace and calls in the value namespace, so the
// same path works for both.
pub use lesto_macros::{delete, get, head, options, patch, post, put, views};

pub use axum;
pub use axum::extract::{Extension, State};
pub use axum::http;
pub use axum::http::StatusCode;
pub use garde;
pub use schemars;
pub use serde_json;

/// Everything a typical handler module needs.
pub mod prelude {
    pub use crate::openapi::{ApiKeyIn, OAuthFlows, SecurityScheme};
    pub use crate::security::{ApiKey, ApiKeyScheme, AuthScheme, Basic, Bearer, Security};
    pub use crate::{
        App, ErrorFormat, Extension, HttpError, Json, Path, Query, State, StatusCode, routes,
    };
    pub use garde::Validate;
    pub use schemars::JsonSchema;
    pub use serde::{Deserialize, Serialize};
}

/// Collect annotated handlers into a [`RouteSet`].
///
/// ```ignore
/// App::new().routes(routes![create_user, get_user])
/// ```
#[macro_export]
macro_rules! routes {
    ($($handler:path),* $(,)?) => {{
        let set = $crate::RouteSet::new();
        $(
            <$handler>::__lesto_check(&set);
            let set = set.add(<$handler as $crate::RouteInfo>::meta(), $handler);
        )*
        set
    }};
}

/// Compile-time checks emitted by the route macros. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    use axum::extract::FromRequest;
    use axum::response::IntoResponse;

    /// Every argument except the last must be a parts extractor.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot be a handler argument in this position (state type `{S}`)",
        label = "not a `FromRequestParts<{S}>` extractor",
        note = "extractors that consume the request body (`Json<T>`, `String`, `Bytes`, `Request`) must be the LAST argument of the handler",
        note = "if `{Self}` is a custom extractor, implement `axum::extract::FromRequestParts<S>` for it (see the tutorial, chapter 8)",
        note = "the state type `{S}` is the `S` of the `App<S>` this `routes![]` set is added to; if that is not the state the extractor expects, check `with_state(..)` / `App::<S>::new()`, or pin it with `#[lesto::get(\"/path\", state = AppState)]`"
    )]
    pub trait PartsExtractor<S> {}
    impl<S, T: FromRequestParts<S>> PartsExtractor<S> for T {}

    /// The last argument may be a parts extractor or a body extractor.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` is not an extractor (state type `{S}`)",
        label = "does not implement `FromRequest<{S}>` or `FromRequestParts<{S}>`",
        note = "request data goes through `lesto::Json<T>`, `lesto::Query<T>` or `lesto::Path<T>`; `T` needs `#[derive(Deserialize, JsonSchema)]`, plus `#[derive(Validate)]` for `Json`/`Query`",
        note = "if `{Self}` is a custom extractor, implement `axum::extract::FromRequestParts<S>` for it (see the tutorial, chapter 8)"
    )]
    pub trait LastExtractor<S, M> {}
    impl<S, M, T: FromRequest<S, M>> LastExtractor<S, M> for T {}

    /// The return type must be a response.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot be returned from a handler",
        label = "does not implement `axum::response::IntoResponse`",
        note = "wrap JSON data in `lesto::Json<T>` (with `T: Serialize + JsonSchema`), or return `String`, `&'static str`, `()`, `StatusCode`, `Html<T>`, or `Result<T, lesto::HttpError>` of those"
    )]
    pub trait HandlerResponse {}
    impl<T: IntoResponse> HandlerResponse for T {}

    // Also used by `#[derive(Store)]` (feature `db`).
    pub use crate::{OperationBuilder, OperationInput};
    pub use axum::extract::FromRequestParts;
    pub use http::request::Parts;

    pub fn check_parts<T: PartsExtractor<S>, S: Send + Sync>() {}
    pub fn check_last<T: LastExtractor<S, M>, S: Send + Sync, M>() {}
    pub fn check_response<T: HandlerResponse>() {}
    pub fn check_documented_input<T: crate::OperationInput>() {}
    pub fn check_documented_output<T: crate::OperationOutput>() {}
}

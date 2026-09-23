# D. Leaving lesto

lesto is a layer on axum, not a replacement for it, so leaving is a sequence of small steps, each
of which leaves a working application behind. Nobody is locked in; this appendix shows the way
out so that you can judge the way in.

## 1. Take the router

`into_router()` turns an `App` into an ordinary `axum::Router`, with the documentation pages,
the problem fallbacks and every layer already installed. Serve it with axum:

```rust
let router: lesto::axum::Router = build_app(state).into_router();
let listener = lesto::tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
lesto::axum::serve(listener, router)
    .with_graceful_shutdown(lesto::shutdown_signal())
    .await?;
```

From here on, new code can be plain axum: `router.merge(..)`, `router.nest(..)`, any tower layer.

## 2. Write handlers without the route attribute

A handler without `#[lesto::post]` is an axum handler. lesto's extractors and errors still work
in it, because they are ordinary `FromRequest` and `IntoResponse` types:

```rust
/// A plain axum handler: no route attribute, still lesto's validating `Json` and `HttpError`.
async fn plain_create_item(Json(item): Json<Item>) -> Result<Json<ItemOut>, HttpError> {
    if item.name == "forbidden" {
        return Err(HttpError::forbidden("that name is taken"));
    }
    Ok(Json(ItemOut {
        name: item.name,
        price_with_tax: item.price + item.tax.unwrap_or(0.0),
    }))
}
```

What it loses is the documentation: axum does not keep a handler's types, so the operation is
missing from `openapi.json`. Freeze the document before you go (`lesto openapi -o openapi.json`)
and serve the file, or keep the documented part of the API on lesto while the rest moves.

## 3. Keep the layers

A router that `App` never built can keep lesto's runtime behavior with the public layers:

```rust
pub fn plain_router() -> lesto::axum::Router {
    use lesto::layers::{CatchPanicLayer, ProblemLayer, RequestSpanLayer, TimeoutLayer};
    use std::time::Duration;

    lesto::axum::Router::new()
        .route("/items", lesto::axum::routing::post(plain_create_item))
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .layer(CatchPanicLayer)
        .layer(ProblemLayer)
        .layer(RequestSpanLayer::new())
}
```

- `ProblemLayer` fills in `instance` on every RFC 9457 response.
- `CatchPanicLayer` answers a panic with a `500` problem instead of a dropped connection.
- `RequestSpanLayer` opens the request span of chapter 15.
- `TimeoutLayer` answers `503` when a request runs too long.
- CORS, compression and request ids are tower-http's own layers (`CorsLayer`,
  `CompressionLayer`, `SetRequestIdLayer` + `PropagateRequestIdLayer`): `App::cors` and friends
  only install them.

The order is the one `App` uses: the last layer added is the outermost.

## 4. Replace the derives

`#[lesto::model]` expands to four derives through lesto's copies of serde, schemars and garde
(chapter 4). Write them out and add the three crates to your manifest:

```rust
#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema, garde::Validate)]
struct Item { /* the fields, unchanged */ }
```

A model that had no garde rule needs `#[garde(allow_unvalidated)]`, which `#[lesto::model]` added
for you. `#[lesto::views]` keeps working on plain derives, or its output can be written by hand.

## 5. The last pieces

- `#[lesto::main]` / `#[lesto::test]` become `#[tokio::main]` / `#[tokio::test]` with tokio in
  the manifest.
- `lesto::Json` / `lesto::Query` validate with garde and answer RFC 9457 problems. Keep them
  (they have no dependency on `App`), or replace them with `axum::Json` and a call to
  `.validate()` in the handler.
- `lesto::db` stores are plain structs around a sqlx pool; the closure bodies are sqlx code and
  move as they are.

At that point `lesto` can leave `Cargo.toml`.

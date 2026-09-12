# B. Common problems

## `cannot find `garde` in the crate root` / `use of unresolved module or unlinked crate `schemars``

The `Validate` and `JsonSchema` derives generate code that refers to `::garde` and `::schemars`.
Your crate must declare them as direct dependencies:

```toml
garde = { version = "0.23", features = ["derive"] }
schemars = { version = "1", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
```

## `field `x` has no validation rule`

garde requires an attribute on every field. Add `#[garde(skip)]` to the field or
`#[garde(allow_unvalidated)]` to the struct.

## `the trait bound `Foo: Validate` is not satisfied`

You are using `Json<Foo>` or `Query<Foo>` with a type lacking `#[derive(Validate)]`. Add it, or if
`Foo` is an external type use `lesto::axum::Json<Foo>` (documented but without garde validation).

## `the trait bound `Foo: JsonSchema` is not satisfied`

`#[derive(JsonSchema)]` is missing on a type that goes through the API (body, response, query,
path). For external types that do not implement it, wrap them in a newtype or use
`#[schemars(with = "...")]`.

## `X cannot be a handler argument in this position`

A body-consuming extractor (`Json<T>`, `String`, `Bytes`, `Request`) is not the last argument.
Move it to the end. The error points at the offending argument.

## `X is not an extractor (state type S)`

The last argument is not usable as an extractor with that state. Typical causes:

1. The payload type lacks a derive: `Deserialize` and `JsonSchema` for `Path`/`Query`/`Json`,
   plus `Validate` for `Json`/`Query`.
2. The extractor only works with a specific state and the handler is registered on an `App<S>`
   with another one. The checks take the state from a `State<T>` argument if there is one, else
   from the `App<S>` the `routes![]` set is added to. `#[lesto::get("/path", state = AppState)]`
   pins it explicitly, which moves the error onto the offending argument.

## `lesto cannot document X as a handler argument / response`

The type works at runtime but lesto does not know how to describe it in OpenAPI. For payloads,
derive `JsonSchema` and use `Json<T>`/`Query<T>`/`Path<T>`; for a custom extractor or response
type, add `impl lesto::OperationInput for X {}` / `impl lesto::OperationOutput for X {}`
(an empty impl documents nothing) or delegate to the wrapped extractor (chapter 8).

## `the trait bound `fn(...) {handler}: Handler<_, _>` is not satisfied`

axum's own error; lesto's per-argument checks usually report the real cause just above it. If
it appears alone, check: the state in `State<A>` matches `with_state(B)`; every handler in the
same `routes![]` uses the same state; the handler is `async` and `Send` (no `std` `MutexGuard`
held across an `.await`).

## `type annotations needed for `App<_>``

The state type cannot be inferred (for instance an `App` with no `State` handlers, on which
`.openapi()` is called before `with_state`). Write `App::<()>::new()` or `App::<AppState>::new()`.

## `expected type, found function `list`` in `routes![list]`

The handler is missing its `#[lesto::get(...)]` attribute: `routes!` looks for the metadata
type the attribute generates, and finds only the function. Add the attribute.

## `routes![users::list]` cannot find the function

The handler must be `pub` (the generated type has the same visibility as the function) and the
module must expose it.

## Swagger UI says "Could not render responses"

A schema is not an object. Recent lesto versions convert boolean schemas; if it happens with a
type of yours that implements `JsonSchema` by hand, return an object (`{}` instead of `true`).

## The port is already in use

`Address already in use`: another process is listening on the same port. Change port or find the
process with `lsof -i :8000`.

## Fields in the documentation are not in the struct's order

With the current lesto version the order is preserved (schemars' `preserve_order` feature). If
you use schemars directly elsewhere, enable the same feature for consistency.

## I want FastAPI's error format

`App::new().error_format(ErrorFormat::FastApi)`.

## `/docs` is blank behind a firewall

The documentation pages load Scalar and Swagger UI from jsDelivr (pinned versions, integrity
hashes). Where the CDN is unreachable, host the files yourself and point the pages at them:
`App::scalar_script_url("https://cdn.internal/scalar.js")` and
`App::swagger_ui_base_url("https://cdn.internal/swagger-ui-dist")` (the directory that holds
`swagger-ui.css` and `swagger-ui-bundle.js`).

## Client generators complain about duplicate `operationId`s

Every operation gets `{function}_{path}_{method}` by default, which is unique per route. If you
set `operation_id = "..."` by hand, the values must be unique across the whole document, nested
apps included.

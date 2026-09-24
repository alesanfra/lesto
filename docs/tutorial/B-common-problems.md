# B. Common problems

## `cannot find `garde` in the crate root` / `use of unresolved module or unlinked crate `schemars``

The plain derives (`#[derive(Validate)]`, `#[derive(JsonSchema)]`, `#[derive(Serialize)]`)
generate code that refers to `::garde`, `::schemars` and `::serde`, which only exist when your
crate depends on them. Use `#[lesto::model]` instead: it derives the same traits through lesto's
re-exports. If you do want the plain derives, add the three crates to your manifest, at the
versions lesto uses (`garde` 0.23, `schemars` 1, `serde` 1).

## warning: `axum::Json<T>` / `axum::extract::Query<T>` does not run `T`'s garde rules

The handler takes axum's `Json` or `Query`, not lesto's, and `T` has garde rules: the payload
reaches the handler without being validated. Import `lesto::Json` / `lesto::Query` (they are in
`lesto::prelude`). If skipping validation is what you want, put `#[allow(deprecated)]` on the
handler: lesto reports this as a deprecation because that is the one warning a macro can raise.

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

The type works at runtime but lesto does not know how to describe it in OpenAPI. For a response
type this is always an error; for an argument only with lesto's `strict-docs` feature on (or
when registering through `RouteSet::add` instead of `routes![]`). For payloads, derive
`JsonSchema` (`#[lesto::model]`) and use `Json<T>`/`Query<T>`/`Path<T>`; for a custom extractor
or response type, add `impl lesto::OperationInput for X {}` / `impl lesto::OperationOutput for X {}`
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

They are: lesto guarantees declaration order for JSON keys, in the OpenAPI document and in
response bodies (struct fields, problem extensions, a `serde_json::Map` filled in order). It does
so by enabling `serde_json` and `schemars`' `preserve_order` feature. Cargo unifies features
across a build, so every `serde_json::Map` in your application is insertion-ordered too, not
sorted by key. If a field is out of order, it is almost always a `HashMap`, which has no order to
keep: use `indexmap::IndexMap` (or `BTreeMap` for sorted keys).

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

## A `Jwt` route answers `500`

The log says ``a `Jwt` argument needs a verifier``: the app serving the route has no
`App::oidc(..)` (or `App::protect(..)`). Call `.oidc(auth)` on the app you serve; it covers the
nested apps too (chapter 9).

## Every token answers `401` "The token is meant for another audience" (or "another issuer")

The provider puts in `aud` (or `iss`) something other than what lesto expects. Decode one token
(the middle segment is base64 JSON) and compare: `audiences([..])` must list a value of its `aud`;
`iss` must equal the `issuer` of the discovery document, which is why the discovery URL has to be
the provider's own (behind a proxy, the provider must announce the public URL). The reason is
logged at `debug` by `lesto`: `RUST_LOG=lesto=debug` shows it when a subscriber is installed
(chapter 15).

## `MCP resources are not supported yet` (or prompts)

Only tools can be exposed over MCP so far: write `mcp = "tool"`. A `GET` route that an agent
should read is a read-only tool (chapter 16).

## `panicked at 'two routes are exposed as the MCP tool `x`'`

Two routes carry `mcp(tool, name = "x")` with the same name. Tool names must be unique across
the app, nested apps included: rename one. Default names (the handler's) never collide: when
two exposed handlers share a name, both use their `operationId`.

## An MCP client says the server rejected the request with `403`

The request came from a browser page on another origin. The endpoint refuses foreign `Origin`s
against DNS rebinding; list the page's origin in `Mcp::allowed_origins([..])`.

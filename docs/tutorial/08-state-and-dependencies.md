# 8. State and dependencies

Almost every API needs something shared across requests: a database pool, configuration, an HTTP
client. And you often want to reuse extraction logic, such as "the authenticated user". In FastAPI
both go through `Depends`; in lesto, as in axum, they go through **extractors**.

## Shared state: `State<S>`

Declare a type for the state; it must be `Clone` (it usually holds `Arc`s):

```rust
use std::sync::{Arc, Mutex};
use lesto::prelude::*;

#[derive(Clone, Default)]
struct AppState {
    counter: Arc<Mutex<u64>>,
}

/// Increment and return the counter.
#[lesto::post("/hits")]
async fn hit(State(state): State<AppState>) -> String {
    let mut n = state.counter.lock().unwrap();
    *n += 1;
    n.to_string()
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    App::new()
        .routes(routes![hit])
        .with_state(AppState::default())
        .serve()
        .await
}
```

`State<AppState>` in the handler says "I want the state"; `.with_state(...)` on the `App` provides
it. The compiler checks that the two types match: forget `with_state`, or pass a different type,
and the program does not compile. `State` does not appear in the documentation, because it does
not come from the client.

In tests and larger projects an explicit type helps: `App::<AppState>::new()`.

## Sub-states with `FromRef`

If a handler only wants the pool and not the whole state, axum offers `FromRef`:

```rust
use lesto::axum::extract::FromRef;

#[derive(Clone)]
struct AppState {
    db: DbPool,
    config: Arc<Config>,
}

impl FromRef<AppState> for DbPool {
    fn from_ref(s: &AppState) -> DbPool { s.db.clone() }
}

async fn handler(State(db): State<DbPool>) { .. }     // works with with_state(AppState)
```

With axum's `#[derive(FromRef)]` (`macros` feature) the impl is generated for every field.

## `Extension<T>`

For values inserted by a middleware (a request id, a user authenticated by an external layer)
there is `Extension<T>`, also invisible in the documentation:

```rust
async fn handler(Extension(req_id): Extension<RequestId>) { .. }
```

## Custom extractors: lesto's `Depends`

An extractor is a type that knows how to build itself from the request. Writing one means
implementing `FromRequestParts`, and optionally `OperationInput` to tell the documentation what it
requires.

The classic example: the current user derived from the Bearer token.

```rust
use lesto::axum::extract::FromRequestParts;
use lesto::http::request::Parts;
use lesto::prelude::*;
use lesto::{BearerAuth, OperationBuilder, OperationInput, Rejection};

#[derive(Clone, Serialize, JsonSchema)]
pub struct User {
    pub id: u64,
    pub name: String,
}

/// The authenticated user. Use it as a handler argument.
pub struct CurrentUser(pub User);

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Rejection> {
        // Reuse the Bearer extractor: missing header → 401 with WWW-Authenticate.
        let auth = Bearer::<BearerAuth>::from_request_parts(parts, state).await?;
        // This is where you would verify the token (JWT, session, ...).
        match auth.token() {
            "secret" => Ok(CurrentUser(User { id: 1, name: "Ada".into() })),
            _ => Err(HttpError::unauthorized("invalid token").into()),
        }
    }
}

// The documentation inherits Bearer's: bearerAuth scheme + 401 response.
impl OperationInput for CurrentUser {
    fn describe(builder: &mut OperationBuilder<'_>) {
        Bearer::<BearerAuth>::describe(builder);
    }
}

/// Profile of the authenticated user.
#[lesto::get("/me")]
async fn me(CurrentUser(user): CurrentUser) -> Json<User> {
    Json(user)
}
```

```sh
curl http://127.0.0.1:8000/me -H 'Authorization: Bearer secret'
```

```json
{"id":1,"name":"Ada"}
```

Every handler that takes `CurrentUser` as an argument gets an already verified user, and its
documentation shows the lock icon. It plays the same role as
`user: User = Depends(get_current_user)`.

If the extractor has nothing to do with the client (it only reads state or extensions), leave
`impl OperationInput for MyType {}` with an empty body: the default documents nothing.

## Dependencies with parameters

In FastAPI a dependency can itself take query parameters. In lesto you compose extractors inside
your own:

```rust
pub struct Page { pub offset: usize, pub limit: usize }

impl<S: Send + Sync> FromRequestParts<S> for Page {
    type Rejection = Rejection;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Rejection> {
        let Query(p) = Query::<Pagination>::from_request_parts(parts, state).await?;
        Ok(Page { offset: p.skip, limit: p.limit })
    }
}

impl OperationInput for Page {
    fn describe(builder: &mut OperationBuilder<'_>) {
        Query::<Pagination>::describe(builder);     // documents skip and limit
    }
}
```

## Recap

- `State<S>` + `with_state(s)`: shared state, checked by the compiler.
- `Extension<T>` for values inserted by middleware.
- A custom extractor = `FromRequestParts` (+ `OperationInput` for the documentation). Extractors
  compose: `Bearer`, `Query`, `State` inside yours.

Next: [Security](09-security.md).

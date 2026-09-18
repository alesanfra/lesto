# 10. Bigger projects

A single `main.rs` is fine for a tutorial; a real API is split into modules. Here is a layout that
works well.

The runnable version of this chapter is `examples/05-routers`: a shop serving two APIs from one
process, `/api/app/v1` (products and orders) and `/api/analytics/v1` (sales figures), each a
module with its own `router()`, sharing one state and one OpenAPI document
(`LESTO_PORT=8765 cargo run -p routers`, then open `/docs`).

```
src/
├── main.rs          # builds the App, mounts the modules, starts
├── state.rs         # AppState
├── errors.rs        # From<RepoError> for HttpError, domain error types
├── auth.rs          # CurrentUser, security schemes
├── users/
│   ├── mod.rs       # pub fn routes() -> RouteSet<AppState>
│   ├── models.rs    # UserIn, UserOut
│   └── handlers.rs  # the annotated handlers
└── items/
    └── ...
```

## A route module

Every module exposes a function returning its own routes:

```rust
// src/users/handlers.rs
use lesto::prelude::*;
use crate::state::AppState;
use super::models::{UserIn, UserOut};

/// List users.
#[lesto::get("/", tag = "users")]
pub async fn list(State(state): State<AppState>) -> Json<Vec<UserOut>> { .. }

/// Create a user.
#[lesto::post("/", status = 201, tag = "users", responses(409))]
pub async fn create(State(state): State<AppState>, Json(body): Json<UserIn>) -> Result<Json<UserOut>, HttpError> { .. }

/// One user by id.
#[lesto::get("/{id}", tag = "users", responses(404))]
pub async fn get(State(state): State<AppState>, Path(id): Path<u64>) -> Result<Json<UserOut>, HttpError> { .. }
```

```rust
// src/users/mod.rs
mod handlers;
pub mod models;

use lesto::{routes, RouteSet};
use crate::state::AppState;

pub fn routes() -> RouteSet<AppState> {
    routes![handlers::list, handlers::create, handlers::get]
}
```

Handlers are `pub` because `routes!` references them from another module. The attribute generates,
next to the function, a type with the same name and visibility carrying the route metadata: that
is why `routes![handlers::list]` works with any path.

## Mounting modules under a prefix

```rust
// src/main.rs
mod auth;
mod errors;
mod items;
mod state;
mod users;

use lesto::prelude::*;
use state::AppState;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let users = App::<AppState>::new().routes(users::routes());
    let items = App::<AppState>::new().routes(items::routes());

    App::new()
        .title("Shop API")
        .version("1.0.0")
        .tag("users", "User management")
        .tag("items", "Catalog")
        .nest("/users", users)
        .nest("/items", items)
        .with_state(AppState::new().await)
        .serve() // LESTO_HOST=0.0.0.0 in a container
        .await
}
```

`nest("/users", app)` is FastAPI's `include_router(router, prefix="/users")`: the module's routes
become `/users` (the module's `/` route) and `/users/{id}`, and the documentation is merged with
the right prefix. Only the root App serves `/docs` and `/openapi.json`.

If you prefer to avoid the prefix and write full paths in the handlers, you can also merge the
routes directly: `App::new().routes(users::routes()).routes(items::routes())`.

## Tags

`tag = "users"` on the attribute groups operations in the documentation. `App::tag(name,
description)` adds the group's description. An operation can have several tags:
`tags("users", "admin")`.

## Versioning the API

```rust
App::new()
    .nest("/v1", v1_app)
    .nest("/v2", v2_app)
```

## One `App` per environment

Since `App` is a value, you can build it in a function and reuse it in tests:

```rust
pub fn build_app(state: AppState) -> App<()> {
    App::new()
        .title("Shop API")
        .nest("/users", App::new().routes(users::routes()))
        .with_state(state)
}
```

Chapter 12 shows how to test it without opening a port.

## Recap

- One module per resource, with `pub fn routes() -> RouteSet<AppState>`.
- `pub` handlers referenced by path in `routes![]`.
- `nest(prefix, app)` to mount and prefix; `tag` to group.

Next: [Middleware and axum](11-middleware-and-axum.md).

# 13. Databases: stores and principals

The `db` feature of lesto connects [sqlx](https://github.com/launchbadge/sqlx) to
lesto. It adds one idea, the **store**: a handler argument that carries a database handle and
the **principal** (who is calling), and runs each of its methods inside its own transaction.

```rust
#[lesto::get("/notes")]
async fn list_notes(store: NoteStore<ReadOnly, Public>) -> Result<Json<Vec<Note>>, Error> {
    Ok(Json(store.list().await?))
}

#[lesto::post("/notes", status = 201)]
async fn create_note(
    store: NoteStore<ReadWrite, User>,
    Json(body): Json<NoteCreate>,
) -> Result<Json<Note>, Error> {
    Ok(Json(store.create(body).await?))
}
```

Read the two signatures as sentences: "a read-only store held by anyone" and "a read-write store
held by an authenticated user". Everything else follows from the types: the 401 when the token is
missing, the 403 when a permission is missing, the security scheme in the documentation, the
read-only transaction for the first handler and the read-write one for the second.

The full code of this chapter is `examples/02-notes` in the repository (`LESTO_PORT=8765 cargo run -p notes`),
split the way a real project would be: `state.rs`, `auth.rs`, and `notes/{model,store,handlers}.rs`,
with end-to-end tests in `tests/api.rs`. It also has a `PATCH` route built on a `NoteUpdate` view.

## Setup

```toml
[dependencies]
lesto = { path = "../lesto/crates/lesto", features = ["sqlite"] }   # or "postgres", "mysql"
sqlx = { version = "0.9", default-features = false, features = ["runtime-tokio", "sqlite", "derive"] }
```

`sqlite`, `postgres` and `mysql` each imply the `db` feature and the matching sqlx driver.

The database handle lives in your state. `Db` holds the primary pool and, optionally, a read
replica; stores fetch it with axum's `FromRef`:

```rust
use lesto::axum::extract::FromRef;
use lesto::db::prelude::*;
use sqlx::Sqlite;

#[derive(Clone)]
pub struct AppState {
    pub db: Db<Sqlite>,
    pub tokens: Tokens,
}

impl FromRef<AppState> for Db<Sqlite> {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}
```

```rust
let state = AppState { db: Db::new(pool), tokens: demo_tokens() };
App::<AppState>::new().routes(routes![...]).with_state(state)
```

With `Db::new(primary).with_replica(replica)`, read-only stores use the replica.

## The principal: who is calling

A **principal** is whoever holds the store. Two kinds exist:

- `Public`: anyone, no credentials, never has permissions.
- your own type, for which you implement **`Authenticated`**. That is the only trait you write.

```rust
#[derive(Debug, Clone)]
pub struct User {
    pub name: String,
    pub permissions: Vec<String>,
}

impl Authenticated for User {
    type State = AppState;
    type Credential = Bearer;                          // chapter 9: any security extractor

    async fn authenticate(token: Bearer, state: &AppState) -> Result<Self, HttpError> {
        state
            .tokens
            .get(token.token())
            .cloned()
            .ok_or_else(|| HttpError::unauthorized("Unknown token"))
    }

    fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }
}
```

- `Credential` is the extractor that reads the raw credential: `Bearer`, `ApiKey<MyKey>`, `Basic`
  or your own. Its behavior is inherited: the 401 when it is missing, the `WWW-Authenticate`
  header, the security scheme in OpenAPI.
- `authenticate` turns the credential into an identity. Here a lookup table; in a real
  application, a JWT check or a session query. Failing with `HttpError::unauthorized` answers 401.
- `has_permission` is asked before every store method that declares a permission.

`State` is an associated type, so a principal belongs to one application state. `Public` works
with any state.

## The store

A store is a **newtype** around `lesto::db::Store<M, P, DB>` with `#[derive(Store)]`. The
derive forwards extraction and documentation to the inner type and adds `Deref`, so the store's
methods are available on your type:

```rust
#[derive(lesto::db::Store)]
pub struct NoteStore<M, P>(Store<M, P, Sqlite>);
```

`M` is the **mode** (`ReadOnly` or `ReadWrite`), `P` the principal. You write the queries as
methods, in impl blocks whose bounds say when they are available:

```rust
/// Reads: any mode, any principal.
impl<M: Mode, P> NoteStore<M, P> {
    pub async fn list(&self) -> Result<Vec<Note>, Error> {
        self.read(Anyone, async |conn| {
            sqlx::query_as("SELECT id, author, text FROM notes ORDER BY id")
                .fetch_all(conn)
                .await
        })
        .await
    }

    pub async fn get(&self, id: i64) -> Result<Note, Error> {
        self.read(Anyone, async |conn| {
            sqlx::query_as("SELECT id, author, text FROM notes WHERE id = ?")
                .bind(id)
                .fetch_one(conn) // RowNotFound → 404
                .await
        })
        .await
    }
}

/// Writes: read-write stores held by a `User`.
impl<M: Writable> NoteStore<M, User> {
    pub async fn create(&self, body: NoteCreate) -> Result<Note, Error> {
        let author = self.principal().name.clone();
        self.write("notes:write", async |conn| {
            sqlx::query_as(
                "INSERT INTO notes (author, text) VALUES (?, ?) RETURNING id, author, text",
            )
            .bind(&author)
            .bind(&body.text)
            .fetch_one(conn)
            .await
        })
        .await
    }

    pub async fn delete(&self, id: i64) -> Result<(), Error> {
        self.write("notes:delete", async |conn| {
            let result = sqlx::query("DELETE FROM notes WHERE id = ?")
                .bind(id)
                .execute(conn)
                .await?;
            if result.rows_affected() == 0 {
                return Err(Error::not_found("No such note"));
            }
            Ok(())
        })
        .await
    }
}
```

`read` and `write` do the same four things: check the **requirement** (first argument), open a
transaction, run your closure with the connection, commit. If the closure returns an error, the
transaction is rolled back and the error becomes the response. You never write `begin` or
`commit`.

- `read` opens a **read-only** transaction (`BEGIN READ ONLY` on Postgres, `START TRANSACTION
  READ ONLY` on MySQL, `BEGIN DEFERRED` on SQLite, which has no read-only mode) on the replica
  when there is one.
- `write` opens a read-write transaction on the primary, and only compiles when `M: Writable`,
  that is, on `ReadWrite` stores. Calling `write` from a `ReadOnly` store is a compile error that
  says so.

The closure receives `&mut SqliteConnection` (or `PgConnection`, ...), so `fetch_all(conn)` works
as in any sqlx code. The closure may return any error that converts into `lesto::db::Error`:
`sqlx::Error` (from `?`), `HttpError`, or the constructors `Error::not_found(..)`,
`Error::conflict(..)`, `Error::bad_request(..)`. For other error types use
`result.internal()?` (500, cause logged) or the `anyhow` feature.

### Permissions

The first argument of `read`/`write` is a `Requirement`:

- `Anyone`: no check.
- a string such as `"notes:write"`: the principal must answer `true` from `has_permission`,
  otherwise the method fails with **403** before the transaction opens:

```json
{"type":"about:blank","title":"Forbidden","status":403,
 "detail":"Missing permission `notes:write`","instance":"/notes","required_permission":"notes:write"}
```

A permission string on a `Public` store does not compile: `Public` can never hold permissions.
The error message says to pass `Anyone` or to declare an authenticated principal in the handler.
For your own combinators (any of, all of), implement `Requirement<P>`.

### Why two impl blocks

The first block is generic over the principal, so `list` and `get` are available on both
`NoteStore<_, Public>` and `NoteStore<_, User>`. The second is pinned to `User`, because
`create` needs `self.principal().name` and a permission. Rust's type system does the routing:
a handler with `NoteStore<ReadOnly, Public>` simply has no `create` method.

## The model

Nothing new: a struct with serde, schemars, garde and `sqlx::FromRow`. The `#[lesto::views]`
macro from chapter 6 gives you the request body for free:

```rust
#[lesto::views(Create(text))]
#[derive(Debug, Serialize, Deserialize, JsonSchema, Validate, sqlx::FromRow)]
pub struct Note {
    #[garde(skip)]
    pub id: i64,
    #[garde(skip)]
    pub author: String,
    /// The note itself.
    #[garde(length(min = 1, max = 280))]
    pub text: String,
}
```

## Errors

Handlers return `Result<_, lesto::db::Error>`. The error answers as an RFC 9457 problem and
documents itself:

| Cause | Response |
|---|---|
| missing permission | 403, with `required_permission` |
| `fetch_one` found no row (`RowNotFound`) | 404 |
| unique or foreign key violation | 409 |
| `Error::not_found(..)`, `Error::conflict(..)`, any `HttpError` | that error |
| other sqlx errors, `.internal()`, `anyhow::Error` | 500, detail hidden, cause logged with `tracing` |

In OpenAPI the operation gets 403, 404, 409 and `default` responses with the `Problem` schema,
plus 401 and the security scheme when the principal is authenticated.

## Trying it

```sh
LESTO_PORT=8765 cargo run -p notes
curl -s localhost:8765/notes
# []
curl -s -X POST localhost:8765/notes -H 'Authorization: Bearer bob-token' \
     -H 'Content-Type: application/json' -d '{"text":"hello"}'
# {"id":1,"author":"bob","text":"hello"}
curl -s -X DELETE localhost:8765/notes/1 -H 'Authorization: Bearer bob-token'
# {"type":"about:blank","title":"Forbidden","status":403,"detail":"Missing permission `notes:delete`",
#  "instance":"/notes/1","required_permission":"notes:delete"}
curl -s -X DELETE localhost:8765/notes/1 -H 'Authorization: Bearer alice-token' -o /dev/null -w '%{http_code}\n'
# 204
```

## Testing

Stores are plain values: `NoteStore(Store::new(db, Public))` or `Store::new(db, some_user)`
builds one by hand, without HTTP. For end-to-end tests use `oneshot` as in chapter 12. With
SQLite in memory keep **one connection** in the pool: every connection to `sqlite::memory:` is a
separate database.

```rust
let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await?;
```

## Recap

- `Db<DB>` in the state, reachable through `FromRef`.
- Implement `Authenticated` once; `Public` is built in.
- `#[derive(Store)]` on a newtype; queries as methods that call `self.read(requirement, ..)` or
  `self.write(requirement, ..)`.
- `ReadOnly` / `ReadWrite` and `Anyone` / `"permission"` are checked by the compiler where
  possible and before the transaction otherwise.
- `lesto::db::Error` maps database errors to 403, 404, 409, 500 and documents them.

Next: [Deploying to AWS Lambda](14-aws-lambda.md).

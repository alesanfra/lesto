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

sqlx is the one dependency lesto cannot absorb. Its query functions are all reachable through
`lesto::db::sqlx` (`lesto::db::sqlx::query_as(..)`), but `#[derive(sqlx::FromRow)]` and the
`query!` macros generate code that names `::sqlx` and offer no way to point it elsewhere, so a
crate that uses them lists sqlx itself. Keep the version in step with lesto's (0.9): two sqlx
versions in one build are two incompatible sets of types. Without `FromRow` (tuples, or a
hand-written impl) the line can go.

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
  It runs **once per request**, however many stores the handler takes: the first store to be
  extracted keeps the principal in the request extensions and the others share it
  (`store.principal()` still hands out a `&User`; `into_principal()` gives the `Arc<User>`).
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
                .fetch_one(conn)
                .await
                .or_not_found("no such note")
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

### Declaring the permission pair once

Passing the permission at every call site means every call site can pass the wrong one. Declare
the pair on the store instead:

```rust
#[derive(lesto::db::Store)]
#[store(read = "notes:read", write = "notes:write")]
struct NoteStore<M, P>(Store<M, P, Sqlite>);
```

`read`, `read_with`, `write` and `write_with` are then generated **without the requirement
argument** — the permission comes from the type, and a method cannot name the wrong one because
there is nowhere to name it:

```rust
impl<M: Writable> NoteStore<M, User> {
    async fn create(&self, body: NoteCreate) -> Result<Note, Error> {
        self.write(async |conn| { /* ... */ }).await   // "notes:write", always
    }
}
```

Declare only `read` for a store that has no writes, only `write` for one whose reads are public.
Whatever you leave undeclared keeps the explicit two-argument form, so a store can mix the two.
The generated methods need an authenticated principal, since a permission does: on a `Public`
store use `Anyone` and the explicit form.

### Why two impl blocks

The first block is generic over the principal, so `list` and `get` are available on both
`NoteStore<_, Public>` and `NoteStore<_, User>`. The second is pinned to `User`, because
`create` needs `self.principal().name` and a permission. Rust's type system does the routing:
a handler with `NoteStore<ReadOnly, Public>` simply has no `create` method.

## Row level security: publishing the identity

Sometimes the check belongs in the database, not in the query. Postgres row level security lets
a policy decide which rows exist for the caller — but the database has to be told who the caller
is. That is `transaction_settings`, on the principal:

```rust
impl Authenticated for User {
    // ...credential, authenticate, has_permission as above...

    fn transaction_settings(&self) -> TransactionSettings {
        TransactionSettings::empty()
            .set("app.user_id", self.id)          // a Uuid
            .set_opt("app.organization_id", self.organization_id)
            .set("app.is_staff", self.is_staff)   // a bool
    }
}
```

Every transaction this principal opens now starts with:

```sql
BEGIN READ ONLY; SET LOCAL app.user_id = '...'; SET LOCAL app.organization_id = '...'; SET LOCAL app.is_staff = 'false'
```

sent as **one round trip**, and the policy reads it back:

```sql
CREATE POLICY notes_read ON notes FOR SELECT USING (
    owner = nullif(current_setting('app.user_id', true), '')::uuid
    OR coalesce(nullif(current_setting('app.is_staff', true), '')::boolean, false)
);
```

The store method then has no `WHERE owner = $1` at all — the policy is the filter:

```rust
async fn list(&self) -> Result<Vec<Note>, Error> {
    self.read(Anyone, async |conn| {
        sqlx::query_as("SELECT id, owner, text FROM notes").fetch_all(conn).await
    })
    .await
}
```

`SET LOCAL` ends with the transaction, so nothing is left on the pooled connection: the next
request that gets that connection sees no identity, and a `Public` store on it reads nothing.
`Public` publishes no settings, and a principal that does not implement the method publishes
none either — the default is empty, so nothing changes until you ask for it.

### What a value may be

`set` takes a `bool`, an integer, or a `Uuid`. That is the whole list, and it is short on
purpose: `SET LOCAL` accepts no bind parameters on any database, so every value here is
interpolated into SQL. Those three have text forms that cannot contain a quote, which is what
makes the interpolation safe; a `String` would not, so there is no way to pass one. Setting
names are `&'static str` — from your source, never from a request — and their characters are
checked as well.

`set_opt` writes the empty string for `None`, which is why the policy above reads it back
through `nullif(..., '')`. Names need a dotted prefix (`app.user_id`): Postgres rejects an
undotted custom setting.

### Postgres only

MySQL has no transaction-scoped setting and SQLite has none at all, so a principal that
publishes settings on either fails the request with a **500** rather than running the query with
the identity quietly missing. If you are on SQLite — as the `notes` example is — return
`TransactionSettings::empty()` and filter in the query.

## One method per use case

A store method is named after what the *caller* wants to do, not after the query it runs or the
table it touches. The handler calls it once:

```rust
#[lesto::post("/claims", status = 201)]
async fn create_claim(
    store: ClaimStore<ReadWrite, User>,
    Json(body): Json<ClaimCreate>,
) -> Result<Json<Claim>, Error> {
    Ok(Json(store.claim(body).await?))
}
```

The shape to avoid is a handler that orchestrates:

```rust
// Don't: three methods modelled on the queries, and the handler deciding.
if !store.claimable(body.practitioner_id).await? {   // transaction 1
    return Err(Error::conflict("Already claimed"));
}
if store.caller_owns_a_profile().await? {            // transaction 2
    return Err(Error::conflict("You already have a profile"));
}
Ok(Json(store.create(body).await?))                  // transaction 3
```

Each method opens its own transaction, so the two checks have stopped being true by the time the
third one writes — the race from the previous section, reintroduced by the shape of the code
rather than by the isolation level. Moving both checks inside `claim`'s own `write` closure is
what makes them mean something.

The question to ask of a handler: **if two identical requests arrive at the same instant, does
this handler read a value that the other one could change before my write?** If yes, that read
and that write belong in one closure, and therefore in one method.

Several calls are fine when they are independent — two unrelated reads to build one response
page. There you are paying latency, not correctness.

## Never read on the replica to decide a write

With `Db::new(primary).with_replica(replica)`, `read` goes to the replica and `write` to the
primary. So this is worse than it looks:

```rust
// Don't: the check reads a replica that may be behind, the write lands on the primary.
if store.is_available(id).await? {
    store.reserve(id).await?;
}
```

The check is not merely one transaction behind the write — it is behind by the replication lag,
which is unbounded in principle. A read whose result decides a write goes **inside the `write`
closure**, where it runs on the primary and inside the same transaction:

```rust
self.write("bookings:create", async |conn| {
    let available: bool = sqlx::query_scalar("SELECT .. FOR UPDATE").bind(id)
        .fetch_one(&mut *conn).await?;
    if !available {
        return Err(Error::conflict("Already reserved"));
    }
    // ... the INSERT ...
})
.await
```

This holds even with no replica configured: writing it this way means adding one later does not
silently turn a correct handler into a racy one.

## Concurrency: when a check has to hold

A store method that reads a row to decide whether to write has a race in it, and one
transaction is not enough to close it. At the default isolation another request can read the
same row, reach the same decision, and write too — both transactions are correct on their own
and the result is wrong.

Two ways out. Prefer the first.

### A unique constraint

```sql
CREATE UNIQUE INDEX one_claim_per_practitioner ON claims (practitioner_id);
```

The loser's `INSERT` fails, and `lesto::db::Error` already answers **409 Already exists** for a
unique violation — no code to write. This protects the table against every writer, not only
against the ones that go through this method, which is why it is the better answer whenever the
rule can be expressed as a constraint.

### `Isolation::Serializable`

When the rule cannot be a constraint — it depends on rows in another table, or on the *absence*
of rows — ask the database to serialize the transaction:

```rust
impl<M: Writable> ClaimStore<M, User> {
    async fn claim(&self, body: ClaimCreate) -> Result<Claim, Error> {
        self.write_with("claims:submit", Isolation::Serializable, async |conn| {
            // The check and the write it decides are one serializable unit: whatever this
            // reads still holds when the transaction commits.
            let taken: bool = sqlx::query_scalar(
                "SELECT exists (SELECT 1 FROM claims WHERE practitioner_id = $1)",
            )
            .bind(body.practitioner_id)
            .fetch_one(&mut *conn)
            .await?;
            if taken {
                return Err(Error::conflict("Already claimed"));
            }
            sqlx::query_as("INSERT INTO claims (..) VALUES (..) RETURNING ..")
                .fetch_one(conn)
                .await
                .map_err(Error::from)
        })
        .await
    }
}
```

`read_with` is the same for reads, where `Isolation::Snapshot` is usually what you want: several
queries in one transaction seeing one stable picture.

| | What it gives you |
|---|---|
| `Isolation::Default` | what the database does on its own. No conflicts to handle. |
| `Isolation::Snapshot` | one stable snapshot for the transaction. Not enough for check-then-act. |
| `Isolation::Serializable` | a check made inside the transaction still holds at commit. |

Each renders to whatever the database needs — `ISOLATION LEVEL ...` on Postgres and MySQL,
`BEGIN IMMEDIATE` on SQLite, which has no isolation levels because it only ever provides one.
Nothing is refused anywhere.

### The conflict is a 409, not a 500

Under `Snapshot` and `Serializable` the database can refuse a transaction instead of making it
wait. lesto answers that with **409** and `Retry-After: 0`:

```json
{"type":"about:blank","title":"Conflict","status":409,
 "detail":"Conflicting concurrent change, retry the request","instance":"/claims"}
```

The request is safe to send again — nothing about it was wrong, it lost a race.

lesto retries for you first. `read_with` and `write_with` re-run the closure on a transient
conflict, twice by default; the 409 is what a client sees once the budget is spent. Set the
budget on the handle:

```rust
Db::new(primary).with_conflict_retries(4)   // 0 disables retrying
```

It lives there, not in a fourth argument, because it is policy rather than a per-call decision.

This is also why `read_with` and `write_with` take an `AsyncFn` where `read` and `write` take an
`AsyncFnOnce`: **their closure may run more than once.** Keep effects the outside world can see
— an email, a queue publish — out of it, or the retry does them twice. `read` and `write` never
retry, because at the database's own isolation there is no conflict to retry, so the common case
keeps the weaker bound.

Lock *timeouts* are deliberately not in this bucket. A `ER_LOCK_WAIT_TIMEOUT` or a Postgres
`55P03` means somebody held a lock too long, which retrying does not fix, so those stay 500s and
stay visible.

## The model

Nothing new: a struct with serde, schemars, garde and `sqlx::FromRow`. The `#[lesto::views]`
macro from chapter 6 gives you the request body for free:

```rust
#[lesto::model(views(Create(text)))]
#[derive(Debug, sqlx::FromRow)]
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
| `.or_not_found(..)` on a query that found no row | 404 |
| unique or foreign key violation | 409 |
| serialization failure or deadlock (`40001`, `40P01`, `SQLITE_BUSY`) | 409 with `Retry-After: 0` |
| `Error::not_found(..)`, `Error::conflict(..)`, any `HttpError` | that error |
| other sqlx errors (a bare `RowNotFound` too), `.internal()`, `anyhow::Error` | 500, detail hidden, cause logged with `tracing` |

A `fetch_one` that finds nothing is a 500 unless you say otherwise. Only some lookups mean "the
resource does not exist": the note named in the path, yes; the author row a note points at, no —
that one missing is a bug, and a 404 would hide it. Mark the first kind with
`.or_not_found("no such note")` (from the prelude); on `fetch_optional`, `None` becomes the 404
the same way.

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

## Several methods, one transaction

Each method opens its own transaction, so two method calls are two transactions. When one use
case genuinely needs several pieces of SQL under one commit, share the *query*, not the method:
write it against a connection somebody else opened, using the `Connection<DB>` alias.

```rust
impl<M: Mode, P> NoteStore<M, P> {
    // A query, on a connection it did not open.
    async fn count_in(conn: &mut Connection<Sqlite>, author: &str) -> Result<i64, Error> {
        sqlx::query_scalar("SELECT count(*) FROM notes WHERE author = ?")
            .bind(author).fetch_one(conn).await.map_err(Error::from)
    }
}

impl<M: Writable> NoteStore<M, User> {
    // The use case, opening one transaction for all of it.
    async fn create_unless_full(&self, body: NoteCreate) -> Result<Note, Error> {
        let author = self.principal().name.clone();
        self.write("notes:write", async |conn| {
            if Self::count_in(conn, &author).await? >= 100 {
                return Err(Error::conflict("Too many notes"));
            }
            // ... the INSERT, on the same conn, in the same transaction ...
        })
        .await
    }
}
```

Two different stores can take part the same way: both helpers take `&mut Connection<Sqlite>`, so
whichever store opened the transaction lends its connection to the other. What there is no way
to do is call two *public* store methods and have them share a transaction — each one opens its
own, by design.

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
- `transaction_settings` on the principal publishes the caller's identity to every transaction
  (`SET LOCAL`, Postgres only), for row level security policies.
- A check that decides a write needs a unique constraint or `Isolation::Serializable` through
  `read_with` / `write_with`; the resulting conflict is a 409 with `Retry-After`.
- One store method per use case, and the check that decides a write lives inside that write's
  closure — never in a separate `read`, which would run on the replica.
- `#[store(read = "..", write = "..")]` declares the permission pair on the type; the generated
  `read`/`write` then take no requirement argument.
- `read_with`/`write_with` retry a conflict `Db::with_conflict_retries` times before the 409,
  which is why their closure is an `AsyncFn`.

Next: [Deploying to AWS Lambda](14-aws-lambda.md).

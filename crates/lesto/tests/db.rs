//! End-to-end: a notes API on SQLite with a `Public` and a `User` principal.

use http_body_util::BodyExt;
use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::axum::extract::FromRef;
use lesto::db::Dialect;
use lesto::db::prelude::*;
use lesto::db::uuid::Uuid;
use lesto::http::{Request, StatusCode, header};
use lesto::prelude::*;
use serde_json::{Value, json};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};
use tower::ServiceExt;

// ---- state and principal --------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    db: Db<Sqlite>,
}

impl FromRef<AppState> for Db<Sqlite> {
    fn from_ref(s: &AppState) -> Self {
        s.db.clone()
    }
}

/// Demo identity: the bearer token is `name|perm1,perm2`. `bad` is rejected.
#[derive(Debug)]
struct User {
    name: String,
    permissions: Vec<String>,
}

impl Authenticated for User {
    type State = AppState;
    type Credential = Bearer;

    async fn authenticate(token: Bearer, _state: &AppState) -> Result<Self, HttpError> {
        let (name, perms) = token
            .token()
            .split_once('|')
            .ok_or_else(|| HttpError::unauthorized("Invalid token"))?;
        Ok(User {
            name: name.to_string(),
            permissions: perms
                .split(',')
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect(),
        })
    }

    fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }
}

// ---- model and store ------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
struct Note {
    id: i64,
    author: String,
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct NoteCreate {
    #[garde(length(min = 1))]
    text: String,
}

#[derive(lesto::db::Store)]
struct NoteStore<M, P>(Store<M, P, Sqlite>);

impl<M: Mode, P> NoteStore<M, P> {
    async fn list(&self) -> Result<Vec<Note>, Error> {
        self.read(Anyone, async |conn| {
            sqlx::query_as("SELECT id, author, text FROM notes ORDER BY id")
                .fetch_all(conn)
                .await
        })
        .await
    }

    async fn get(&self, id: i64) -> Result<Note, Error> {
        self.read(Anyone, async |conn| {
            sqlx::query_as("SELECT id, author, text FROM notes WHERE id = ?")
                .bind(id)
                .fetch_one(conn)
                .await
        })
        .await
    }
}

impl<M: Writable> NoteStore<M, User> {
    async fn create(&self, body: NoteCreate) -> Result<Note, Error> {
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

    async fn delete(&self, id: i64) -> Result<(), Error> {
        self.write("notes:delete", async |conn| {
            let done = sqlx::query("DELETE FROM notes WHERE id = ?")
                .bind(id)
                .execute(conn)
                .await?;
            if done.rows_affected() == 0 {
                return Err(Error::not_found("No such note"));
            }
            Ok(())
        })
        .await
    }

    /// Inserts, then fails: the insert must be rolled back.
    async fn create_then_fail(&self) -> Result<(), Error> {
        self.write(Anyone, async |conn| {
            sqlx::query("INSERT INTO notes (author, text) VALUES ('ghost', 'boo')")
                .execute(conn)
                .await?;
            Err::<(), _>(Error::bad_request("Changed my mind"))
        })
        .await
    }

    /// A non-sqlx error through `.internal()`.
    async fn parse_failure(&self) -> Result<i32, Error> {
        self.write(Anyone, async |_conn| "nope".parse::<i32>().internal())
            .await
    }
}

// ---- handlers -------------------------------------------------------------------------------

#[lesto::get("/notes")]
async fn list_notes(store: NoteStore<ReadOnly, Public>) -> Result<Json<Vec<Note>>, Error> {
    Ok(Json(store.list().await?))
}

#[lesto::get("/notes/{id}")]
async fn get_note(
    store: NoteStore<ReadOnly, Public>,
    Path(id): Path<i64>,
) -> Result<Json<Note>, Error> {
    Ok(Json(store.get(id).await?))
}

#[lesto::post("/notes", status = 201)]
async fn create_note(
    store: NoteStore<ReadWrite, User>,
    Json(body): Json<NoteCreate>,
) -> Result<Json<Note>, Error> {
    Ok(Json(store.create(body).await?))
}

#[lesto::delete("/notes/{id}", status = 204)]
async fn delete_note(store: NoteStore<ReadWrite, User>, Path(id): Path<i64>) -> Result<(), Error> {
    store.delete(id).await
}

#[lesto::post("/boom")]
async fn boom(store: NoteStore<ReadWrite, User>) -> Result<(), Error> {
    store.create_then_fail().await
}

#[lesto::post("/parse")]
async fn parse(store: NoteStore<ReadWrite, User>) -> Result<String, Error> {
    Ok(store.parse_failure().await?.to_string())
}

// ---- helpers --------------------------------------------------------------------------------

async fn pool(seed: &[&str]) -> Pool<Sqlite> {
    // Every connection to `sqlite::memory:` is its own database: keep a single connection.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, author TEXT NOT NULL, text TEXT NOT NULL UNIQUE)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for text in seed {
        sqlx::query("INSERT INTO notes (author, text) VALUES ('seed', ?)")
            .bind(text)
            .execute(&pool)
            .await
            .unwrap();
    }
    pool
}

fn app() -> App<AppState> {
    App::<AppState>::new().title("Notes").routes(routes![
        list_notes,
        get_note,
        create_note,
        delete_note,
        boom,
        parse
    ])
}

async fn router(db: Db<Sqlite>) -> Router {
    app().with_state(AppState { db }).into_router()
}

async fn send(router: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn post(uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut req = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    req.body(Body::from(body.to_string())).unwrap()
}

fn delete(uri: &str, token: &str) -> Request<Body> {
    Request::delete(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

// ---- tests ----------------------------------------------------------------------------------

#[tokio::test]
async fn public_read_and_authenticated_write() {
    let router = router(Db::new(pool(&["first"]).await)).await;

    let (status, body) = send(&router, get("/notes")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["text"], "first");

    let (status, body) = send(
        &router,
        post(
            "/notes",
            Some("alice|notes:write"),
            json!({"text": "second"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["author"], "alice");
    assert_eq!(body["id"], 2);

    let (status, body) = send(&router, get("/notes/2")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "second");
}

#[tokio::test]
async fn missing_token_is_401_and_bad_token_too() {
    let router = router(Db::new(pool(&[]).await)).await;

    let (status, body) = send(&router, post("/notes", None, json!({"text": "x"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["status"], 401);

    let (status, body) = send(&router, post("/notes", Some("bad"), json!({"text": "x"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["detail"], "Invalid token");
}

#[tokio::test]
async fn missing_permission_is_403_with_required_permission() {
    let router = router(Db::new(pool(&[]).await)).await;

    let (status, body) = send(
        &router,
        post("/notes", Some("bob|notes:read"), json!({"text": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["required_permission"], "notes:write");
    assert_eq!(body["detail"], "Missing permission `notes:write`");
    assert_eq!(body["instance"], "/notes");

    // Nothing was written.
    let (_, body) = send(&router, get("/notes")).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn row_not_found_is_404_and_unique_violation_is_409() {
    let router = router(Db::new(pool(&["dup"]).await)).await;

    let (status, body) = send(&router, get("/notes/99")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["title"], "Not Found");

    let (status, body) = send(
        &router,
        post("/notes", Some("alice|notes:write"), json!({"text": "dup"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["detail"], "Already exists");
}

#[tokio::test]
async fn http_error_from_closure_and_delete() {
    let router = router(Db::new(pool(&["gone"]).await)).await;

    let (status, _) = send(&router, delete("/notes/1", "alice|notes:delete")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send(&router, delete("/notes/1", "alice|notes:delete")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "No such note");

    let (status, body) = send(&router, delete("/notes/1", "alice|notes:write")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["required_permission"], "notes:delete");
}

#[tokio::test]
async fn failed_write_rolls_back() {
    let router = router(Db::new(pool(&[]).await)).await;

    let (status, body) = send(&router, post("/boom", Some("alice|"), json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "Changed my mind");

    let (_, body) = send(&router, get("/notes")).await;
    assert_eq!(body, json!([]));
}

#[tokio::test]
async fn internal_errors_are_500_without_details() {
    let router = router(Db::new(pool(&[]).await)).await;

    let (status, body) = send(&router, post("/parse", Some("alice|"), json!({}))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["detail"], "Internal Server Error");
    assert!(!body.to_string().contains("invalid digit"));
}

#[tokio::test]
async fn reads_go_to_the_replica_and_writes_to_the_primary() {
    let primary = pool(&["primary-note"]).await;
    let replica = pool(&["replica-note"]).await;
    let router = router(Db::new(primary.clone()).with_replica(replica)).await;

    let (_, body) = send(&router, get("/notes")).await;
    assert_eq!(body[0]["text"], "replica-note");

    let (status, _) = send(
        &router,
        post("/notes", Some("alice|notes:write"), json!({"text": "new"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notes")
        .fetch_one(&primary)
        .await
        .unwrap();
    assert_eq!(n, 2);
    let (_, body) = send(&router, get("/notes")).await;
    assert_eq!(body.as_array().unwrap().len(), 1, "replica untouched");
}

#[tokio::test]
async fn openapi_documents_security_and_errors() {
    let spec = app().openapi_json();
    let spec: Value = serde_json::from_str(&spec).unwrap();

    let list = &spec["paths"]["/notes"]["get"];
    assert!(
        list.get("security").is_none(),
        "public route has no security"
    );
    assert!(list["responses"].get("401").is_none());
    assert!(
        list["responses"].get("403").is_some(),
        "Error documents 403"
    );
    assert!(list["responses"].get("404").is_some());
    assert!(list["responses"].get("409").is_some());
    assert_eq!(
        list["responses"]["200"]["content"]["application/json"]["schema"]["type"],
        "array"
    );

    let create = &spec["paths"]["/notes"]["post"];
    assert_eq!(create["security"], json!([{"bearerAuth": []}]));
    assert!(create["responses"].get("401").is_some());
    assert!(create["responses"].get("403").is_some());
    assert!(create["responses"].get("422").is_some());
    assert_eq!(
        create["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/Note"
    );
    assert_eq!(
        spec["components"]["securitySchemes"]["bearerAuth"]["type"],
        "http"
    );
}

#[tokio::test]
async fn store_can_be_built_by_hand() {
    let db = Db::new(pool(&["a", "b"]).await);
    let store = NoteStore(Store::<ReadOnly, _, _>::new(db, Public));
    assert_eq!(store.list().await.unwrap().len(), 2);

    let user = User {
        name: "carol".into(),
        permissions: vec!["notes:write".into()],
    };
    let store = NoteStore(Store::<ReadWrite, _, _>::new(store.db().clone(), user));
    let note = store.create(NoteCreate { text: "c".into() }).await.unwrap();
    assert_eq!(note.author, "carol");
    assert_eq!(store.principal().name, "carol");
}

#[test]
fn error_status_mapping() {
    assert_eq!(
        Error::Forbidden { permission: "x" }.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        Error::from(sqlx::Error::RowNotFound).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        Error::from(sqlx::Error::PoolClosed).status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        Error::from(HttpError::conflict("x")).status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        Error::from(anyhow::anyhow!("boom")).status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let e: Error = "nope".parse::<i32>().internal().unwrap_err();
    assert!(matches!(e, Error::Internal(_)));
    assert_eq!(<Sqlite as Dialect>::BEGIN_READ_ONLY, "BEGIN DEFERRED");
}

// ---- transaction settings -------------------------------------------------------------------

/// A principal that publishes its identity to every transaction it opens. SQLite has no
/// transaction-local settings, so it is here to prove that lesto says so instead of running the
/// queries with the identity silently missing. The Postgres end of this lives in
/// `tests/db_postgres.rs`.
#[derive(Debug)]
struct Tenant {
    id: Uuid,
}

impl Authenticated for Tenant {
    type State = AppState;
    type Credential = Bearer;

    async fn authenticate(_token: Bearer, _state: &AppState) -> Result<Self, HttpError> {
        Ok(Tenant { id: Uuid::nil() })
    }

    fn has_permission(&self, _permission: &str) -> bool {
        true
    }

    fn transaction_settings(&self) -> TransactionSettings {
        TransactionSettings::empty()
            .set("app.user_id", self.id)
            .set("app.is_staff", false)
    }
}

#[tokio::test]
async fn a_principal_without_settings_opens_the_plain_transaction() {
    let store = NoteStore(Store::<ReadOnly, _, _>::new(
        Db::new(pool(&["a"]).await),
        Public,
    ));
    assert!(store.settings().is_empty());
    assert_eq!(store.list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn settings_on_a_database_that_has_none_fail_the_request() {
    let store = NoteStore(Store::<ReadOnly, _, _>::new(
        Db::new(pool(&["a"]).await),
        Tenant { id: Uuid::nil() },
    ));
    assert_eq!(store.settings().len(), 2);

    let error = store.list().await.unwrap_err();
    assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    // The cause names the database and what to do; the response body never carries it.
    assert!(
        error
            .to_string()
            .contains("has no transaction-local settings"),
        "{error}"
    );
}

// ---- isolation ------------------------------------------------------------------------------

#[tokio::test]
async fn a_serializable_write_opens_begin_immediate_on_sqlite() {
    let store = NoteStore(Store::<ReadWrite, _, _>::new(
        Db::new(pool(&[]).await),
        User {
            name: "alice".into(),
            permissions: vec![],
        },
    ));

    // SQLite has no isolation levels; `Serializable` takes the write lock up front instead, so
    // what this proves is that `BEGIN IMMEDIATE` is what SQLite gets and that it accepts it.
    let count = store
        .write_with(Anyone, Isolation::Serializable, async |conn| {
            sqlx::query("INSERT INTO notes (author, text) VALUES ('alice', 'serialized')")
                .execute(&mut *conn)
                .await?;
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notes")
                .fetch_one(conn)
                .await
        })
        .await
        .unwrap();
    assert_eq!(count, 1);

    // And a read at every isolation still works.
    for isolation in [
        Isolation::Default,
        Isolation::Snapshot,
        Isolation::Serializable,
    ] {
        let rows = store
            .read_with(Anyone, isolation, async |conn| {
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notes")
                    .fetch_one(conn)
                    .await
            })
            .await
            .unwrap();
        assert_eq!(rows, 1, "{isolation:?}");
    }
}


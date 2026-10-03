//! What `lesto::log::Console` prints, in both formats, for requests through a real router.
//!
//! Each test installs the console as its thread's default subscriber, writing into a buffer, so
//! the tests run side by side and nothing reaches the real stdout.

use std::sync::{Arc, Mutex};

use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::axum::extract::FromRef;
use lesto::db::prelude::*;
use lesto::http::{Request, StatusCode};
use lesto::log::{Console, Format};
use lesto::prelude::*;
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

// ---- a buffer the console writes into -------------------------------------------------------

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Buffer {
    fn lines(&self) -> Vec<String> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn json(&self) -> Vec<Value> {
        self.lines()
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
            .collect()
    }
}

impl std::io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buffer {
    type Writer = Buffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn install(format: Format, filter: &str) -> (Buffer, tracing::subscriber::DefaultGuard) {
    let buffer = Buffer::default();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(filter))
        .with(Console::new(format).with_writer(buffer.clone()));
    let guard = tracing::subscriber::set_default(subscriber);
    (buffer, guard)
}

// ---- the application under test -------------------------------------------------------------

#[lesto::get("/users/{id}")]
async fn get_user(Path(id): Path<u64>) -> Json<u64> {
    tracing::warn!(user = id, note = "two words", "looking up a user");
    Json(id)
}

#[lesto::get("/boom")]
async fn boom() -> Result<Json<u64>, HttpError> {
    Err(HttpError::internal("no"))
}

#[lesto::get("/work")]
async fn work() -> &'static str {
    let span = tracing::info_span!("job", id = 3);
    let _entered = span.enter();
    tracing::info!("working");
    "done"
}

fn app() -> Router {
    App::new()
        .routes(routes![get_user, boom, work])
        .openapi_url(None)
        .into_router()
}

async fn call(router: Router, uri: &str) -> StatusCode {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    router.oneshot(request).await.unwrap().status()
}

/// The line without its timestamp, which is all that changes between runs.
fn untimed(line: &str) -> &str {
    line.split_once(' ').map_or(line, |(_, rest)| rest)
}

// ---- text -----------------------------------------------------------------------------------

#[tokio::test]
async fn text_shows_the_request_then_the_event() {
    let (buffer, _guard) = install(Format::Text, "info");
    assert_eq!(call(app(), "/users/7?token=secret").await, StatusCode::OK);

    let lines = buffer.lines();
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(
        untimed(&lines[0]),
        r#" WARN GET /users/7 log: looking up a user user=7 note="two words""#
    );
    let access = untimed(&lines[1]);
    assert!(
        access.starts_with(" INFO GET /users/7 200 ") && !access.contains("lesto::access"),
        "the access log is method, path, status, duration: {access}"
    );
    assert!(
        lines.iter().all(|line| !line.contains("secret")),
        "the query string is never printed: {lines:#?}"
    );
    assert!(
        lines[0].starts_with("20") && lines[0].as_bytes()[10] == b'T',
        "an RFC 3339 timestamp first: {}",
        lines[0]
    );
}

#[tokio::test]
async fn a_server_error_is_an_error_line() {
    let (buffer, _guard) = install(Format::Text, "info");
    call(app(), "/boom").await;

    let lines = buffer.lines();
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        untimed(&lines[0]).starts_with("ERROR GET /boom 500 "),
        "{}",
        lines[0]
    );
}

#[tokio::test]
async fn a_span_of_the_application_shows_its_fields() {
    let (buffer, _guard) = install(Format::Text, "info");
    call(app(), "/work").await;

    let lines = buffer.lines();
    assert_eq!(untimed(&lines[0]), " INFO GET /work job{id=3} log: working");
}

#[tokio::test]
async fn the_access_log_has_a_target_of_its_own() {
    let (buffer, _guard) = install(Format::Text, "info,lesto::access=off");
    call(app(), "/users/7").await;

    let lines = buffer.lines();
    assert_eq!(lines.len(), 1, "only the handler's event: {lines:#?}");
    assert!(lines[0].contains("looking up a user"));
}

#[tokio::test]
async fn trace_off_keeps_the_access_log() {
    let (buffer, _guard) = install(Format::Text, "info");
    let router = App::new()
        .routes(routes![boom])
        .openapi_url(None)
        .trace(Trace::off())
        .into_router();
    call(router, "/boom").await;

    let lines = buffer.lines();
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        untimed(&lines[0]).starts_with("ERROR GET /boom 500 "),
        "{}",
        lines[0]
    );
}

#[tokio::test]
async fn a_route_that_did_not_match_is_logged_with_its_path() {
    let (buffer, _guard) = install(Format::Json, "info");
    call(app(), "/nowhere").await;

    let line = &buffer.json()[0];
    assert_eq!(line["url.path"], "/nowhere");
    assert_eq!(line["http.response.status_code"], 404);
    assert!(line.get("http.route").is_none(), "{line}");
}

// ---- JSON -----------------------------------------------------------------------------------

#[tokio::test]
async fn json_is_one_flat_object_per_line() {
    let (buffer, _guard) = install(Format::Json, "info");
    call(app(), "/users/7?token=secret").await;

    let lines = buffer.json();
    assert_eq!(lines.len(), 2, "{lines:#?}");

    let event = &lines[0];
    assert_eq!(event["level"], "WARN");
    assert_eq!(event["target"], "log");
    assert_eq!(event["message"], "looking up a user");
    assert_eq!(event["http.request.method"], "GET");
    assert_eq!(event["http.route"], "/users/{id}");
    assert_eq!(event["url.path"], "/users/7");
    assert_eq!(event["user"], 7, "numbers stay numbers");
    assert_eq!(event["note"], "two words");
    assert!(
        event.get("user_agent.original").is_none(),
        "only what identifies the request: {event}"
    );

    let access = &lines[1];
    assert_eq!(access["level"], "INFO");
    assert_eq!(access["target"], "lesto::access");
    assert_eq!(access["http.response.status_code"], 200);
    assert!(access["duration_ms"].is_f64(), "{access}");
    assert_eq!(
        access["message"], "GET /users/7 200",
        "the body of an exported log record: a backend shows nothing else by default"
    );

    let raw = buffer.lines();
    assert!(
        raw[0].starts_with(r#"{"timestamp":"#),
        "timestamp, level, target and message come first: {}",
        raw[0]
    );
    assert!(raw.iter().all(|line| !line.contains("secret")));
}

// ---- store spans ----------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    db: Db<Sqlite>,
}

impl FromRef<AppState> for Db<Sqlite> {
    fn from_ref(state: &AppState) -> Self {
        state.db.clone()
    }
}

#[derive(lesto::db::Store)]
struct Notes<M, P>(Store<M, P, Sqlite>);

impl<M: Mode, P> Notes<M, P> {
    async fn count(&self) -> Result<i64, Error> {
        self.read(Anyone, async |conn| {
            tracing::info!("counting");
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notes")
                .fetch_one(conn)
                .await
        })
        .await
    }
}

#[lesto::get("/notes/count", state = AppState)]
async fn count_notes(store: Notes<ReadOnly, Public>) -> Result<Json<i64>, Error> {
    Ok(Json(store.count().await?))
}

async fn notes_app() -> Router {
    // Every `sqlite::memory:` connection is a database of its own, so the pool holds one.
    let pool: Pool<Sqlite> = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("CREATE TABLE notes (id INTEGER PRIMARY KEY, text TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    App::new()
        .routes(routes![count_notes])
        .openapi_url(None)
        .with_state(AppState { db: Db::new(pool) })
        .into_router()
}

#[tokio::test]
async fn a_store_transaction_is_shown_by_its_method() {
    let router = notes_app().await;

    let (buffer, guard) = install(Format::Text, "info");
    call(router.clone(), "/notes/count").await;
    assert_eq!(
        untimed(&buffer.lines()[0]),
        " INFO GET /notes/count Notes::count log: counting"
    );
    drop(guard);

    let (buffer, _guard) = install(Format::Json, "info");
    call(router, "/notes/count").await;
    assert_eq!(buffer.json()[0]["store"], "Notes::count");
}

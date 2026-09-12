//! The spans lesto emits, checked field by field against the OpenTelemetry semantic
//! conventions for HTTP servers and database clients.
//!
//! The subscriber is hand-rolled: `tracing` is enough to record a span and its fields, and the
//! test would otherwise need `tracing-subscriber` only to read back what lesto wrote.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::axum::extract::FromRef;
use lesto::db::prelude::*;
use lesto::http::{Request, StatusCode, header};
use lesto::prelude::*;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::span;

// ---- a subscriber that keeps every span and the fields recorded on it -----------------------

#[derive(Debug, Clone, PartialEq)]
struct Captured {
    name: &'static str,
    parent: Option<u64>,
    fields: BTreeMap<String, String>,
}

impl Captured {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    #[track_caller]
    fn assert_field(&self, name: &str, expected: &str) {
        assert_eq!(
            self.field(name),
            Some(expected),
            "field `{name}` of span `{}`, which recorded {:?}",
            self.name,
            self.fields
        );
    }
}

#[derive(Default)]
struct Spans {
    captured: Vec<(u64, Captured)>,
    /// The spans currently entered, innermost last: the contextual parent of a new span.
    stack: Vec<u64>,
}

#[derive(Clone, Default)]
struct Capture {
    spans: Arc<Mutex<Spans>>,
    next_id: Arc<AtomicU64>,
}

impl Capture {
    fn install() -> (Self, tracing::subscriber::DefaultGuard) {
        let capture = Capture::default();
        let guard = tracing::subscriber::set_default(capture.clone());
        (capture, guard)
    }

    /// The one span with this name, and its id.
    #[track_caller]
    fn only(&self, name: &str) -> (u64, Captured) {
        let spans = self.spans.lock().unwrap();
        let mut found = spans.captured.iter().filter(|(_, s)| s.name == name);
        let span = found.next().unwrap_or_else(|| {
            panic!(
                "no span named `{name}`; captured {:?}",
                spans
                    .captured
                    .iter()
                    .map(|(_, s)| s.name)
                    .collect::<Vec<_>>()
            )
        });
        assert!(found.next().is_none(), "more than one span named `{name}`");
        span.clone()
    }

    fn names(&self) -> Vec<&'static str> {
        let spans = self.spans.lock().unwrap();
        spans.captured.iter().map(|(_, s)| s.name).collect()
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &span::Attributes<'_>) -> span::Id {
        // `Id` refuses zero, so ids start at one.
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let mut fields = BTreeMap::new();
        attrs.record(&mut Recorder(&mut fields));
        let mut spans = self.spans.lock().unwrap();
        let parent = spans.stack.last().copied();
        spans.captured.push((
            id,
            Captured {
                name: attrs.metadata().name(),
                parent,
                fields,
            },
        ));
        span::Id::from_u64(id)
    }

    fn record(&self, id: &span::Id, values: &span::Record<'_>) {
        let mut spans = self.spans.lock().unwrap();
        let Some((_, span)) = spans
            .captured
            .iter_mut()
            .find(|(known, _)| *known == id.into_u64())
        else {
            return;
        };
        values.record(&mut Recorder(&mut span.fields));
    }

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}

    fn enter(&self, id: &span::Id) {
        self.spans.lock().unwrap().stack.push(id.into_u64());
    }

    fn exit(&self, id: &span::Id) {
        let mut spans = self.spans.lock().unwrap();
        if let Some(position) = spans.stack.iter().rposition(|e| *e == id.into_u64()) {
            spans.stack.remove(position);
        }
    }
}

/// Every field ends up as a string: the test compares what a collector would export.
struct Recorder<'a>(&'a mut BTreeMap<String, String>);

impl Visit for Recorder<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

// ---- the application under test -------------------------------------------------------------

#[lesto::get("/users/{id}")]
async fn get_user(Path(id): Path<u64>) -> Json<u64> {
    Json(id)
}

#[lesto::get("/boom")]
async fn boom() -> Result<Json<u64>, HttpError> {
    Err(HttpError::internal("no"))
}

fn app() -> Router {
    App::new()
        .routes(routes![get_user, boom])
        .openapi_url(None)
        .into_router()
}

async fn call(router: Router, request: Request<Body>) -> StatusCode {
    router.oneshot(request).await.unwrap().status()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

// ---- HTTP spans -----------------------------------------------------------------------------

#[tokio::test]
async fn request_span_follows_the_http_conventions() {
    let (capture, _guard) = Capture::install();
    assert_eq!(call(app(), get("/users/7")).await, StatusCode::OK);

    let (_, span) = capture.only("http.server.request");
    span.assert_field("otel.name", "GET /users/{id}");
    span.assert_field("otel.kind", "server");
    span.assert_field("http.request.method", "GET");
    span.assert_field("http.route", "/users/{id}");
    span.assert_field("url.path", "/users/7");
    span.assert_field("url.scheme", "http");
    span.assert_field("http.response.status_code", "200");
    span.assert_field("network.protocol.version", "1.1");
    assert_eq!(span.field("url.query"), None, "the query string is opt-in");
    assert_eq!(
        span.field("otel.status_code"),
        None,
        "a 2xx leaves the span status unset"
    );
    assert_eq!(span.field("error.type"), None);
}

#[tokio::test]
async fn a_request_that_matched_no_route_is_named_after_the_method_alone() {
    let (capture, _guard) = Capture::install();
    assert_eq!(call(app(), get("/nowhere")).await, StatusCode::NOT_FOUND);

    let (_, span) = capture.only("http.server.request");
    span.assert_field("otel.name", "GET");
    span.assert_field("url.path", "/nowhere");
    span.assert_field("http.response.status_code", "404");
    assert_eq!(
        span.field("http.route"),
        None,
        "there is no route to record, and the path must not become the span name"
    );
    assert_eq!(
        span.field("otel.status_code"),
        None,
        "a 4xx is the client's error, not the server's"
    );
}

#[tokio::test]
async fn a_server_error_sets_the_span_status() {
    let (capture, _guard) = Capture::install();
    assert_eq!(
        call(app(), get("/boom")).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );

    let (_, span) = capture.only("http.server.request");
    span.assert_field("http.response.status_code", "500");
    span.assert_field("error.type", "500");
    span.assert_field("otel.status_code", "ERROR");
}

#[tokio::test]
async fn an_unknown_method_is_other_plus_the_original() {
    let (capture, _guard) = Capture::install();
    let request = Request::builder()
        .method("QUERY")
        .uri("/users/7")
        .body(Body::empty())
        .unwrap();
    call(app(), request).await;

    let (_, span) = capture.only("http.server.request");
    span.assert_field("http.request.method", "_OTHER");
    span.assert_field("http.request.method_original", "QUERY");
    // The path matched a route, the method did not: a 405, still named after the route.
    span.assert_field("otel.name", "_OTHER /users/{id}");
    span.assert_field("http.response.status_code", "405");
}

#[tokio::test]
async fn the_query_string_is_opt_in_and_redacted() {
    let (capture, _guard) = Capture::install();
    let router = App::new()
        .routes(routes![get_user])
        .openapi_url(None)
        .trace(Trace::new().query(true))
        .into_router();
    call(router, get("/users/7?q=lesto&sig=secret")).await;

    let (_, span) = capture.only("http.server.request");
    span.assert_field("url.query", "q=lesto&sig=REDACTED");
}

#[tokio::test]
async fn forwarding_headers_are_read_only_when_trusted() {
    let forwarded = || {
        Request::builder()
            .uri("/users/7")
            .header("x-forwarded-for", "203.0.113.7, 10.0.0.1")
            .header("x-forwarded-proto", "https")
            .header("x-forwarded-host", "api.example.com:443")
            .header(header::HOST, "internal:8000")
            .body(Body::empty())
            .unwrap()
    };

    let (capture, guard) = Capture::install();
    call(app(), forwarded()).await;
    let (_, span) = capture.only("http.server.request");
    span.assert_field("url.scheme", "http");
    span.assert_field("server.address", "internal");
    span.assert_field("server.port", "8000");
    assert_eq!(
        span.field("client.address"),
        None,
        "anybody can send `X-Forwarded-For`"
    );
    drop(guard);

    let (capture, _guard) = Capture::install();
    let router = App::new()
        .routes(routes![get_user])
        .openapi_url(None)
        .trace(Trace::new().forwarded(true))
        .into_router();
    call(router, forwarded()).await;
    let (_, span) = capture.only("http.server.request");
    span.assert_field("url.scheme", "https");
    span.assert_field("server.address", "api.example.com");
    span.assert_field("server.port", "443");
    span.assert_field("client.address", "203.0.113.7");
}

#[tokio::test]
async fn trace_off_emits_nothing() {
    let (capture, _guard) = Capture::install();
    let router = App::new()
        .routes(routes![get_user])
        .openapi_url(None)
        .trace(Trace::off())
        .into_router();
    call(router, get("/users/7")).await;

    assert!(capture.names().is_empty(), "spans: {:?}", capture.names());
}

// ---- store spans ------------------------------------------------------------------------------

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
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notes")
                .fetch_one(conn)
                .await
        })
        .await
    }

    async fn broken(&self) -> Result<i64, Error> {
        self.read(Anyone, async |conn| {
            sqlx::query_scalar::<_, i64>("SELECT nope FROM notes")
                .fetch_one(conn)
                .await
        })
        .await
    }
}

impl<M: Writable, P> Notes<M, P> {
    async fn add(&self, text: &str) -> Result<(), Error> {
        self.write(Anyone, async |conn| {
            sqlx::query("INSERT INTO notes (text) VALUES (?)")
                .bind(text)
                .execute(conn)
                .await
                .map(|_| ())
        })
        .await
    }
}

#[lesto::get("/notes/count", state = AppState)]
async fn count_notes(store: Notes<ReadOnly, Public>) -> Result<Json<i64>, Error> {
    Ok(Json(store.count().await?))
}

#[lesto::post("/notes", status = 201, state = AppState)]
async fn add_note(store: Notes<ReadWrite, Public>) -> Result<StatusCode, Error> {
    store.add("hello").await?;
    Ok(StatusCode::CREATED)
}

#[lesto::get("/notes/broken", state = AppState)]
async fn broken_query(store: Notes<ReadOnly, Public>) -> Result<Json<i64>, Error> {
    Ok(Json(store.broken().await?))
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
        .routes(routes![count_notes, add_note, broken_query])
        .openapi_url(None)
        .with_state(AppState { db: Db::new(pool) })
        .into_router()
}

#[tokio::test]
async fn a_read_is_a_client_span_under_the_request_span() {
    let router = notes_app().await;
    let (capture, _guard) = Capture::install();
    assert_eq!(call(router, get("/notes/count")).await, StatusCode::OK);

    let (request_id, _) = capture.only("http.server.request");
    let (_, span) = capture.only("db.client.operation");
    span.assert_field("otel.kind", "client");
    span.assert_field("db.system.name", "sqlite");
    // SQLite has no read-only transaction, so a read opens a deferred one.
    span.assert_field("otel.name", "BEGIN DEFERRED");
    span.assert_field("db.operation.name", "BEGIN DEFERRED");
    assert_eq!(span.field("error.type"), None);
    assert_eq!(
        span.parent,
        Some(request_id),
        "the transaction belongs to the request that opened it"
    );
}

#[tokio::test]
async fn a_write_names_the_statement_it_opens() {
    let router = notes_app().await;
    let (capture, _guard) = Capture::install();
    let request = Request::builder()
        .method("POST")
        .uri("/notes")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(router, request).await, StatusCode::CREATED);

    let (_, span) = capture.only("db.client.operation");
    span.assert_field("otel.name", "BEGIN");
    span.assert_field("db.operation.name", "BEGIN");
}

#[tokio::test]
async fn a_failed_transaction_records_the_database_code() {
    let router = notes_app().await;
    let (capture, _guard) = Capture::install();
    assert_eq!(
        call(router, get("/notes/broken")).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );

    let (_, span) = capture.only("db.client.operation");
    span.assert_field("otel.status_code", "ERROR");
    // SQLite reports `SQLITE_ERROR` (1) for a query that does not compile.
    span.assert_field("error.type", "1");
    span.assert_field("db.response.status_code", "1");

    let (_, request) = capture.only("http.server.request");
    request.assert_field("http.response.status_code", "500");
    request.assert_field("error.type", "500");
}

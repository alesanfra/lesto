//! `BackgroundTasks` through a real router: the task runs after the response, failures stay
//! out of it, and the task's span hangs under the request's.
//!
//! Tasks go to a process-wide tracker, so the tests here only wait on their own channels.
//! Shutdown, which waits for that tracker, is in `background_shutdown.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::http::{Request, StatusCode};
use lesto::prelude::*;
use tokio::sync::mpsc;
use tower::ServiceExt;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

// ---- a layer that records spans and events ---------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Seen {
    spans: Vec<SpanSeen>,
    events: Vec<EventSeen>,
}

#[derive(Debug, Clone)]
struct SpanSeen {
    id: u64,
    parent: Option<u64>,
    name: String,
    fields: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct EventSeen {
    target: String,
    level: Level,
    fields: HashMap<String, String>,
    /// The `otel.name` of the span the event happened in.
    span: Option<String>,
}

#[derive(Default)]
struct Fields(HashMap<String, String>);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Seen>>);

impl<S: Subscriber + for<'a> LookupSpan<'a>> tracing_subscriber::Layer<S> for Capture {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let parent = ctx
            .span(id)
            .and_then(|s| s.parent())
            .map(|p| p.id().into_u64());
        self.0.lock().unwrap().spans.push(SpanSeen {
            id: id.into_u64(),
            parent,
            name: attrs.metadata().name().into(),
            fields: fields.0,
        });
    }

    fn on_record(&self, id: &Id, values: &tracing::span::Record<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        values.record(&mut fields);
        let mut seen = self.0.lock().unwrap();
        if let Some(span) = seen.spans.iter_mut().find(|s| s.id == id.into_u64()) {
            span.fields.extend(fields.0);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut seen = self.0.lock().unwrap();
        let span = ctx
            .event_span(event)
            .map(|s| s.id().into_u64())
            .and_then(|id| {
                seen.spans
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.fields.get("otel.name").cloned().unwrap_or(s.name.clone()))
            });
        seen.events.push(EventSeen {
            target: event.metadata().target().into(),
            level: *event.metadata().level(),
            fields: fields.0,
            span,
        });
    }
}

impl Capture {
    fn seen(&self) -> Seen {
        self.0.lock().unwrap().clone()
    }
}

fn install() -> (Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
    (capture, guard)
}

// ---- the application under test --------------------------------------------------------------

type Sender = mpsc::UnboundedSender<&'static str>;

#[derive(Clone)]
struct Probe(Sender);

#[lesto::get("/ok")]
async fn ok(tasks: BackgroundTasks, State(probe): State<Probe>) -> &'static str {
    let tx = probe.0;
    tasks.spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send("ran").unwrap();
    });
    "response"
}

#[lesto::get("/fails")]
async fn fails(tasks: BackgroundTasks, State(probe): State<Probe>) -> &'static str {
    let tx = probe.0;
    tasks.spawn(async move {
        tx.send("started").unwrap();
        Err::<(), _>("the mail server is down")
    });
    "response"
}

#[lesto::get("/panics")]
async fn panics(tasks: BackgroundTasks, State(probe): State<Probe>) -> &'static str {
    let tx = probe.0;
    tasks.spawn(async move {
        tx.send("started").unwrap();
        if tx.send("about to panic").is_ok() {
            panic!("task exploded");
        }
    });
    "response"
}

#[lesto::get("/many")]
async fn many(tasks: BackgroundTasks, State(probe): State<Probe>) -> &'static str {
    for _ in 0..5 {
        let tx = probe.0.clone();
        tasks.spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send("ran").unwrap();
        });
    }
    "response"
}

fn app() -> (Router, mpsc::UnboundedReceiver<&'static str>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let router = App::<Probe>::new()
        .routes(routes![ok, fails, panics, many])
        .with_state::<()>(Probe(tx))
        .into_router();
    (router, rx)
}

async fn get(router: Router, uri: &str) -> (StatusCode, String) {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let body = lesto::axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn next(rx: &mut mpsc::UnboundedReceiver<&'static str>) -> &'static str {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the task ran")
        .unwrap()
}

/// Waits until `capture` has seen an event of `lesto::background`.
async fn logged(capture: &Capture) -> EventSeen {
    for _ in 0..500 {
        let found = capture
            .seen()
            .events
            .into_iter()
            .find(|e| e.target == "lesto::background");
        if let Some(event) = found {
            return event;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing was logged by lesto::background");
}

// ---- tests ------------------------------------------------------------------------------------

#[tokio::test]
async fn a_task_runs_after_the_response() {
    let (router, mut rx) = app();
    let (status, body) = get(router, "/ok").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "response"));
    assert!(
        rx.try_recv().is_err(),
        "the task sleeps: the response did not wait for it"
    );
    assert_eq!(next(&mut rx).await, "ran");
}

#[tokio::test]
async fn several_tasks_all_run() {
    let (router, mut rx) = app();
    assert_eq!(get(router, "/many").await.0, StatusCode::OK);
    for _ in 0..5 {
        assert_eq!(next(&mut rx).await, "ran");
    }
}

#[tokio::test]
async fn an_error_is_logged_and_does_not_touch_the_response() {
    let (capture, _guard) = install();
    let (router, mut rx) = app();
    let (status, body) = get(router, "/fails").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "response"));
    assert_eq!(next(&mut rx).await, "started");
    let event = logged(&capture).await;
    assert_eq!(event.level, Level::ERROR);
    assert_eq!(event.fields["error"], "the mail server is down");
    assert!(
        event
            .span
            .as_deref()
            .is_some_and(|s| s.starts_with("background")),
        "logged inside the task's span: {event:?}"
    );
}

#[tokio::test]
async fn a_panic_is_logged_and_does_not_touch_the_response() {
    let (capture, _guard) = install();
    let (router, mut rx) = app();
    let (status, body) = get(router.clone(), "/panics").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "response"));
    assert_eq!(next(&mut rx).await, "started");
    let event = logged(&capture).await;
    assert_eq!(event.level, Level::ERROR);
    assert_eq!(event.fields["panic"], "task exploded");
    // The app still serves.
    assert_eq!(get(router, "/ok").await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_task_span_is_a_child_of_the_request_span() {
    let (capture, _guard) = install();
    let (router, mut rx) = app();
    assert_eq!(get(router, "/ok").await.0, StatusCode::OK);
    assert_eq!(next(&mut rx).await, "ran");
    let seen = capture.seen();
    let task = seen
        .spans
        .iter()
        .find(|s| s.name == "background")
        .expect("a task span");
    assert_eq!(task.fields["otel.name"], "background background::ok");
    assert_eq!(task.fields["otel.kind"], "internal");
    let parent = seen
        .spans
        .iter()
        .find(|s| Some(s.id) == task.parent)
        .expect("the task has a parent");
    assert!(
        parent.name == "http.server.request",
        "the parent is the request span: {parent:?}"
    );
}

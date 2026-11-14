//! `lesto::lambda` flushes the OpenTelemetry providers after every invocation (features `lambda`
//! and `otel`). A test binary of its own: the providers `lesto::otel::flush` reaches are
//! global, and another test's invocation would flush them too.

use std::time::Duration;

use lesto::lambda::Options;
use lesto::lambda::test::{invoke, invoke_with};
use lesto::otel::Telemetry;
use lesto::prelude::*;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{
    BatchConfigBuilder, BatchSpanProcessor, InMemorySpanExporter, SdkTracerProvider,
};
use serde_json::json;
use tracing_subscriber::layer::SubscriberExt;

#[lesto::get("/ping")]
async fn ping() -> &'static str {
    "pong"
}

fn event() -> String {
    event_with_trace(None)
}

/// The event with the `X-Amzn-Trace-Id` header API Gateway adds when tracing is on.
fn event_with_trace(xray: Option<&str>) -> String {
    let mut headers = json!({ "host": "abc.lambda-url.eu-west-1.on.aws" });
    if let Some(xray) = xray {
        headers["x-amzn-trace-id"] = json!(xray);
    }
    json!({
        "version": "2.0",
        "routeKey": "$default",
        "rawPath": "/ping",
        "rawQueryString": "",
        "headers": headers,
        "requestContext": {
            "accountId": "123456789012",
            "apiId": "abc",
            "domainName": "abc.lambda-url.eu-west-1.on.aws",
            "domainPrefix": "abc",
            "http": {
                "method": "GET",
                "path": "/ping",
                "protocol": "HTTP/1.1",
                "sourceIp": "203.0.113.7",
                "userAgent": "test"
            },
            "requestId": "id",
            "routeKey": "$default",
            "stage": "$default",
            "time": "13/Nov/2026:10:00:00 +0000",
            "timeEpoch": 1_794_564_000_000_u64
        },
        "isBase64Encoded": false
    })
    .to_string()
}

/// The names of the spans the exporter has received so far.
fn exported(exporter: &InMemorySpanExporter) -> Vec<String> {
    exporter
        .get_finished_spans()
        .unwrap()
        .into_iter()
        .map(|span| span.name.into_owned())
        .collect()
}

/// A tracer provider whose batch timer never fires during a test, as in a frozen Lambda
/// environment, so only a flush exports; and a subscriber exporting through it.
fn tracing_to(
    exporter: &InMemorySpanExporter,
) -> (SdkTracerProvider, impl tracing::Subscriber + Send + Sync) {
    let processor = BatchSpanProcessor::builder(exporter.clone())
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_scheduled_delay(Duration::from_secs(3600))
                .build(),
        )
        .build();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(processor)
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    (provider, subscriber)
}

/// The providers `flush` reaches are global: the tests that register some take turns.
static TELEMETRY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn spans_of_an_invocation_are_exported_before_the_next_one() {
    let _turn = TELEMETRY.lock().await;
    let exporter = InMemorySpanExporter::default();
    let (provider, subscriber) = tracing_to(&exporter);
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let telemetry = Telemetry::from_providers(Some(provider), None, None);
    assert!(telemetry.is_exporting_traces());

    let router = App::<()>::new().routes(routes![ping]).into_router();

    // Opted out: the span stays in the batch, as it did before the flush existed.
    let response = invoke_with(&router, &event(), Options::default().flush_telemetry(false))
        .await
        .unwrap();
    assert_eq!(&response.body()[..], b"pong");
    assert!(exported(&exporter).is_empty());

    // The default flushes: everything buffered is exported before `invoke` returns.
    invoke(&router, &event()).await.unwrap();
    assert_eq!(exported(&exporter), ["GET /ping", "GET /ping"]);

    // And the next invocation's span is exported before a third one would start.
    invoke(&router, &event()).await.unwrap();
    assert_eq!(exported(&exporter).len(), 3);

    // Once the guard is gone there is nothing left to flush, and no error either.
    telemetry.shutdown();
    invoke(&router, &event()).await.unwrap();
    lesto::otel::flush();
}

const XRAY: &str =
    "Root=1-5759e988-bd862e3fe1be46a994272793;Parent=53995c3f42cd8ad8;Sampled=1;Lineage=a87bd80c:0";

#[tokio::test]
async fn the_xray_trace_becomes_the_parent_when_asked() {
    let exporter = InMemorySpanExporter::default();
    let (provider, subscriber) = tracing_to(&exporter);
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let router = App::<()>::new().routes(routes![ping]).into_router();

    // No flush per invocation: it would reach the providers the other test registered.
    let plain = Options::default().flush_telemetry(false);
    let xray = plain.xray(true);
    invoke_with(&router, &event_with_trace(Some(XRAY)), xray)
        .await
        .unwrap();
    // Off by default: the header is ignored and the span is a root of its own.
    invoke_with(&router, &event_with_trace(Some(XRAY)), plain)
        .await
        .unwrap();
    // X-Ray chose not to sample: the parent-based sampler records nothing.
    let unsampled = XRAY.replace("Sampled=1", "Sampled=0");
    invoke_with(&router, &event_with_trace(Some(&unsampled)), xray)
        .await
        .unwrap();
    // No header: a root, as without the option.
    invoke_with(&router, &event(), xray).await.unwrap();

    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 3, "the unsampled invocation is not recorded");
    assert_eq!(
        spans[0].span_context.trace_id().to_string(),
        "5759e988bd862e3fe1be46a994272793"
    );
    assert_eq!(spans[0].parent_span_id.to_string(), "53995c3f42cd8ad8");
    assert!(spans[0].parent_span_is_remote);
    for root in &spans[1..] {
        assert_ne!(
            root.span_context.trace_id().to_string(),
            "5759e988bd862e3fe1be46a994272793"
        );
        assert_eq!(root.parent_span_id, opentelemetry::trace::SpanId::INVALID);
    }
}

#[lesto::get("/work")]
async fn work(tasks: BackgroundTasks) -> &'static str {
    tasks.spawn(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
    "queued"
}

#[tokio::test]
async fn a_tasks_span_is_exported_with_its_invocation() {
    let _turn = TELEMETRY.lock().await;
    let exporter = InMemorySpanExporter::default();
    let (provider, subscriber) = tracing_to(&exporter);
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let telemetry = Telemetry::from_providers(Some(provider), None, None);
    let router = App::<()>::new().routes(routes![work]).into_router();

    let event = event().replace("/ping", "/work");
    invoke(&router, &event).await.unwrap();
    let names = exported(&exporter);
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.iter().any(|n| n == "GET /work"), "{names:?}");
    assert!(
        names.iter().any(|n| n.starts_with("background")),
        "{names:?}"
    );

    telemetry.shutdown();
}

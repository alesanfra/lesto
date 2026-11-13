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
    json!({
        "version": "2.0",
        "routeKey": "$default",
        "rawPath": "/ping",
        "rawQueryString": "",
        "headers": { "host": "abc.lambda-url.eu-west-1.on.aws" },
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

#[tokio::test]
async fn spans_of_an_invocation_are_exported_before_the_next_one() {
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

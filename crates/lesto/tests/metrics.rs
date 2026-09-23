//! `http.server.request.duration` (feature `otel`), read back through the SDK's in-memory
//! exporter. A test binary of its own: the meter provider and `enable_metrics` are global.

use lesto::axum::body::Body;
use lesto::http::{Request, StatusCode};
use lesto::prelude::*;
use opentelemetry::Value;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use tower::ServiceExt;

#[lesto::get("/items/{id}")]
async fn item(Path(id): Path<u64>) -> Result<String, HttpError> {
    match id {
        0 => Err(HttpError::internal("broken")),
        id => Ok(format!("item {id}")),
    }
}

/// `(attributes, count)` of every data point of the duration histogram.
fn points(exporter: &InMemoryMetricExporter) -> Vec<(Vec<(String, String)>, u64)> {
    let mut out = Vec::new();
    for resource in exporter.get_finished_metrics().unwrap() {
        for scope in resource.scope_metrics() {
            for metric in scope.metrics() {
                if metric.name() != "http.server.request.duration" {
                    continue;
                }
                assert_eq!(metric.unit(), "s");
                let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() else {
                    panic!("not an f64 histogram: {:?}", metric.data());
                };
                for point in histogram.data_points() {
                    let mut attributes: Vec<_> = point
                        .attributes()
                        .map(|kv| {
                            let value = match &kv.value {
                                Value::I64(v) => v.to_string(),
                                other => other.as_str().into_owned(),
                            };
                            (kv.key.as_str().to_owned(), value)
                        })
                        .collect();
                    attributes.sort();
                    out.push((attributes, point.count()));
                }
            }
        }
    }
    out
}

#[tokio::test]
async fn request_duration_is_recorded_with_route_and_status() {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());
    lesto::otel::enable_metrics();

    let router = App::new().routes(routes![item]).into_router();
    for uri in ["/items/1", "/items/2", "/items/0"] {
        let request = Request::get(uri).body(Body::empty()).unwrap();
        router.clone().oneshot(request).await.unwrap();
    }
    let request = Request::get("/nowhere").body(Body::empty()).unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    provider.force_flush().unwrap();
    let mut points = points(&exporter);
    points.sort();
    let attrs = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    assert_eq!(
        points,
        vec![
            (
                attrs(&[
                    ("error.type", "500"),
                    ("http.request.method", "GET"),
                    ("http.response.status_code", "500"),
                    ("http.route", "/items/{id}"),
                    ("network.protocol.version", "1.1"),
                    ("url.scheme", "http"),
                ]),
                1
            ),
            (
                attrs(&[
                    ("http.request.method", "GET"),
                    ("http.response.status_code", "200"),
                    ("http.route", "/items/{id}"),
                    ("network.protocol.version", "1.1"),
                    ("url.scheme", "http"),
                ]),
                2
            ),
            (
                // No route matched: no `http.route`, as the conventions say.
                attrs(&[
                    ("http.request.method", "GET"),
                    ("http.response.status_code", "404"),
                    ("network.protocol.version", "1.1"),
                    ("url.scheme", "http"),
                ]),
                1
            ),
        ]
    );
}

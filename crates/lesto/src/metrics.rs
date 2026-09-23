//! `http.server.request.duration` (feature `otel`): the one HTTP server metric the semantic
//! conventions mark as required, recorded by [`RequestSpanLayer`](crate::layers::RequestSpanLayer).
//!
//! Recording is gated on an `AtomicBool` that [`crate::otel::enable_metrics`] sets once a meter
//! provider is in place, the same way propagation is: until then a request pays one relaxed load
//! and no clock read, no attribute, no instrument call.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use axum::extract::MatchedPath;
use http::StatusCode;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Histogram;

use crate::trace::Trace;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static DURATION: OnceLock<Histogram<f64>> = OnceLock::new();

/// The bucket boundaries the conventions give for `http.server.request.duration`, in seconds.
const BUCKETS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// Create the instrument on the global meter provider and start recording.
pub(crate) fn enable() {
    DURATION.get_or_init(|| {
        opentelemetry::global::meter("lesto")
            .f64_histogram("http.server.request.duration")
            .with_unit("s")
            .with_description("Duration of HTTP server requests.")
            .with_boundaries(BUCKETS.to_vec())
            .build()
    });
    ACTIVE.store(true, Ordering::Relaxed);
}

/// A request being measured: its start and the attributes known before the response.
#[derive(Debug)]
pub(crate) struct Pending {
    start: Instant,
    attributes: Vec<KeyValue>,
}

/// `None` (and nothing else done) unless metrics are enabled.
pub(crate) fn start<B>(req: &http::Request<B>, config: Trace) -> Option<Pending> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    let mut attributes = Vec::with_capacity(7);
    attributes.push(KeyValue::new(
        "http.request.method",
        crate::trace::method_names(req.method()).0,
    ));
    if let Some(route) = req.extensions().get::<MatchedPath>() {
        attributes.push(KeyValue::new("http.route", route.as_str().to_owned()));
    }
    attributes.push(KeyValue::new(
        "url.scheme",
        crate::trace::scheme(req, config).to_owned(),
    ));
    if let Some(version) = crate::trace::protocol_version(req.version()) {
        attributes.push(KeyValue::new("network.protocol.version", version));
    }
    Some(Pending {
        start: Instant::now(),
        attributes,
    })
}

/// Record the duration with the response status (`None`: the service failed, no response).
pub(crate) fn finish(pending: Pending, status: Option<StatusCode>) {
    let Some(histogram) = DURATION.get() else {
        return;
    };
    let Pending {
        start,
        mut attributes,
    } = pending;
    match status {
        Some(status) => {
            attributes.push(KeyValue::new(
                "http.response.status_code",
                i64::from(status.as_u16()),
            ));
            if status.is_server_error() {
                attributes.push(KeyValue::new("error.type", status.as_str().to_owned()));
            }
        }
        None => attributes.push(KeyValue::new("error.type", "_OTHER")),
    }
    histogram.record(start.elapsed().as_secs_f64(), &attributes);
}

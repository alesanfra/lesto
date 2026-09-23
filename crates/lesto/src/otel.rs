//! OpenTelemetry in one call (feature `otel`): read the standard `OTEL_*` variables, export the
//! spans of [`crate::trace`] **and** the log events over OTLP, print them on the console, and
//! continue traces started upstream.
//!
//! With the feature on, [`App::serve`](crate::App::serve) does it by itself when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set and the application has installed no subscriber of its
//! own, so a `main` that is only
//!
//! ```no_run
//! # use lesto::App;
//! # async fn run(app: App<()>) -> std::io::Result<()> {
//! app.serve().await
//! # }
//! ```
//!
//! exports to a collector as soon as the environment says where:
//!
//! ```sh
//! export OTEL_SERVICE_NAME=notes
//! export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
//! cargo run
//! ```
//!
//! | variable | meaning |
//! |---|---|
//! | `OTEL_EXPORTER_OTLP_ENDPOINT` | where to send telemetry; `/v1/traces`, `/v1/logs` and `/v1/metrics` are appended. Nothing is exported while it is unset |
//! | `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` / `..._LOGS_ENDPOINT` / `..._METRICS_ENDPOINT` | the same, per signal, and used as given |
//! | `OTEL_EXPORTER_OTLP_HEADERS` | `key1=value1,key2=value2`, for a backend that wants an `Authorization` |
//! | `OTEL_SERVICE_NAME` | `service.name` of the exported resource; defaults to the title of the OpenAPI document |
//! | `OTEL_TRACES_EXPORTER=none` | keep the logs, stop exporting spans |
//! | `OTEL_LOGS_EXPORTER=none` | keep the spans, stop exporting logs |
//! | `OTEL_METRICS_EXPORTER=none` | stop exporting metrics (`http.server.request.duration`) |
//! | `OTEL_SDK_DISABLED` | `true` turns every signal off and leaves the console |
//! | `RUST_LOG` | the console and export filter, `info` by default |
//!
//! Metrics: every request records `http.server.request.duration` (a histogram in seconds, with
//! method, route, status, scheme and protocol version), and the meter provider becomes the
//! global one, so the application's own instruments (`opentelemetry::global::meter(..)`) are
//! exported with it.
//!
//! Every `tracing` event (`tracing::info!`, an error logged by lesto, one of your own) becomes
//! an OTLP log record carrying the id of the span it happened in, so a backend shows the logs
//! of a request next to its trace. The console keeps printing them as usual.
//!
//! The export is OTLP over **HTTP/protobuf**. `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` is not
//! honored: build your own exporters and subscriber then (chapter 15 of the tutorial) — lesto
//! stands aside as soon as a subscriber is installed.
//!
//! Call [`init`] by hand where `App::serve` is not the entry point (`lesto::lambda::serve`, a
//! test, a worker), and keep the returned [`Telemetry`] alive: dropping it flushes what is
//! still buffered.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

/// The `OTEL_*` settings lesto itself looks at.
///
/// Everything else (headers, timeouts, the per-signal endpoints) is read by the OTLP exporters
/// from the same environment, so there is nothing for lesto to pass on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
    /// `service.name` of the exported resource.
    pub service_name: String,
    /// An OTLP endpoint is configured, so telemetry can be exported.
    pub has_endpoint: bool,
    /// `OTEL_SDK_DISABLED=true`.
    pub disabled: bool,
    /// `OTEL_TRACES_EXPORTER` is not `none`.
    pub traces: bool,
    /// `OTEL_LOGS_EXPORTER` is not `none`.
    pub logs: bool,
    /// `OTEL_METRICS_EXPORTER` is not `none`.
    pub metrics: bool,
    /// The console filter (`RUST_LOG`), `info` by default.
    pub filter: String,
    /// `OTEL_EXPORTER_OTLP_PROTOCOL`, when it asks for something this feature cannot do.
    pub unsupported_protocol: Option<String>,
}

impl Config {
    /// Read the environment, falling back to `default_service_name` when `OTEL_SERVICE_NAME`
    /// is unset.
    pub fn from_env(default_service_name: &str) -> Self {
        Self::read(default_service_name, |name| std::env::var(name).ok())
    }

    /// [`from_env`](Self::from_env) against any source of variables, which is what makes it
    /// testable: the process environment must not be written to.
    fn read(default_service_name: &str, get: impl Fn(&str) -> Option<String>) -> Self {
        let value = |name: &str| get(name).filter(|v| !v.trim().is_empty());
        let protocol = value("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL")
            .or_else(|| value("OTEL_EXPORTER_OTLP_PROTOCOL"));
        let wanted = |name: &str| value(name).is_none_or(|v| v.trim() != "none");
        Config {
            service_name: value("OTEL_SERVICE_NAME")
                .unwrap_or_else(|| default_service_name.to_string()),
            has_endpoint: value("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
                .or_else(|| value("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"))
                .or_else(|| value("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT"))
                .or_else(|| value("OTEL_EXPORTER_OTLP_ENDPOINT"))
                .is_some(),
            disabled: value("OTEL_SDK_DISABLED").is_some_and(|v| v.trim() == "true"),
            traces: wanted("OTEL_TRACES_EXPORTER"),
            logs: wanted("OTEL_LOGS_EXPORTER"),
            metrics: wanted("OTEL_METRICS_EXPORTER"),
            filter: value("RUST_LOG").unwrap_or_else(|| "info".to_string()),
            unsupported_protocol: protocol.filter(|p| !p.starts_with("http/protobuf")),
        }
    }

    /// Anything at all is exported only when a collector is configured and the SDK is on.
    fn exports(&self) -> bool {
        self.has_endpoint && !self.disabled && (self.traces || self.logs || self.metrics)
    }

    fn exports_metrics(&self) -> bool {
        self.exports() && self.metrics
    }

    fn exports_traces(&self) -> bool {
        self.exports() && self.traces
    }

    fn exports_logs(&self) -> bool {
        self.exports() && self.logs
    }
}

/// The providers installed by [`init`]. Dropping it flushes what is still buffered, so keep it
/// alive for as long as the application runs.
#[derive(Debug)]
#[must_use = "dropping the guard flushes and stops the export; bind it for the lifetime of the application"]
pub struct Telemetry {
    traces: Option<SdkTracerProvider>,
    logs: Option<SdkLoggerProvider>,
    metrics: Option<SdkMeterProvider>,
}

impl Telemetry {
    /// A guard that owns nothing: the console subscriber, or nothing at all.
    fn none() -> Self {
        Telemetry {
            traces: None,
            logs: None,
            metrics: None,
        }
    }

    /// Are spans being exported over OTLP?
    pub fn is_exporting_traces(&self) -> bool {
        self.traces.is_some()
    }

    /// Are log events being exported over OTLP?
    pub fn is_exporting_logs(&self) -> bool {
        self.logs.is_some()
    }

    /// Are metrics (`http.server.request.duration`, and any instrument of the application's
    /// on the global meter provider) being exported over OTLP?
    pub fn is_exporting_metrics(&self) -> bool {
        self.metrics.is_some()
    }

    /// Flush and stop, instead of waiting for the drop.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(provider) = self.traces.take()
            && let Err(error) = provider.shutdown()
        {
            tracing::warn!(%error, "the span exporter did not shut down cleanly");
        }
        if let Some(provider) = self.metrics.take()
            && let Err(error) = provider.shutdown()
        {
            tracing::warn!(%error, "the metric exporter did not shut down cleanly");
        }
        if let Some(provider) = self.logs.take()
            && let Err(error) = provider.shutdown()
        {
            // Printed, not logged: the log pipeline is the thing shutting down.
            eprintln!("lesto: the log exporter did not shut down cleanly: {error}");
        }
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Read the incoming `traceparent` on every request from now on.
///
/// [`init`] and [`init_named`] call this, because they install a propagator themselves. An
/// application that builds its own subscriber (the escape hatch: gRPC, a sampler, another
/// propagator) calls it once, after `opentelemetry::global::set_text_map_propagator`:
///
/// ```no_run
/// opentelemetry::global::set_text_map_propagator(
///     opentelemetry_sdk::propagation::TraceContextPropagator::new(),
/// );
/// lesto::otel::enable_propagation();
/// ```
///
/// Until then lesto does not ask the global propagator anything. The API's default is a no-op
/// that would answer "no parent" to every request, at the price of a lookup per request.
pub fn enable_propagation() {
    crate::trace::propagation::ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Record `http.server.request.duration` on every request from now on, through the global meter
/// provider.
///
/// [`init`] and [`init_named`] call this after installing their meter provider. An application
/// with its own (the escape hatch) calls it once, after `opentelemetry::global::set_meter_provider`:
/// until then lesto records nothing, and a request pays one relaxed load for it.
pub fn enable_metrics() {
    crate::metrics::enable();
}

/// Install the console subscriber and, when the environment points at a collector, the OTLP
/// export of spans and logs plus the W3C trace context propagator.
///
/// The service name is `OTEL_SERVICE_NAME`, or `unknown_service` — the name the conventions
/// ask for when nobody said. [`init_named`] takes a better default.
pub fn init() -> Telemetry {
    init_named("unknown_service")
}

/// [`init`] with the service name to use when `OTEL_SERVICE_NAME` is unset.
pub fn init_named(default_service_name: &str) -> Telemetry {
    install(Config::from_env(default_service_name))
}

/// What [`App::serve`](crate::App::serve) does: nothing at all unless the environment asks for
/// it and the application installed no subscriber of its own.
pub(crate) fn auto_init(default_service_name: &str) -> Option<Telemetry> {
    if tracing::dispatcher::has_been_set() {
        // The application built its own subscriber: it decides where the telemetry goes.
        return None;
    }
    let config = Config::from_env(default_service_name);
    config.exports().then(|| install(config))
}

fn install(config: Config) -> Telemetry {
    // Both providers are built before the subscriber, so a failure can be reported through it
    // afterwards rather than printed to a console nobody configured yet.
    let mut problems = Vec::new();
    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .build();
    let traces = config
        .exports_traces()
        .then(|| tracer_provider(&resource))
        .and_then(|built| unwrap_provider(built, "spans", &mut problems));
    let logs = config
        .exports_logs()
        .then(|| logger_provider(&resource))
        .and_then(|built| unwrap_provider(built, "logs", &mut problems));
    let metrics = config
        .exports_metrics()
        .then(|| meter_provider(&resource))
        .and_then(|built| unwrap_provider(built, "metrics", &mut problems));

    let console =
        tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new(config.filter.clone()))
            .with(tracing_subscriber::fmt::layer())
            .with(traces.as_ref().map(|provider| {
                tracing_opentelemetry::layer()
                    .with_tracer(provider.tracer("lesto"))
                    // Off: the source file, line and module of every span, the thread that
                    // polled it, and its busy/idle timings. Seven attributes per span that say
                    // nothing the span name does not, on every span of every request.
                    .with_location(false)
                    .with_threads(false)
                    .with_tracked_inactivity(false)
            }))
            .with(logs.as_ref().map(|provider| {
                OpenTelemetryTracingBridge::new(provider).with_filter(no_feedback())
            }));
    if console.try_init().is_err() {
        // Another subscriber won the race: exporting through ours would send nothing.
        return Telemetry::none();
    }

    // Continue traces started by the caller. Set after the subscriber so the choice is visible
    // in the logs of whoever wonders why a `traceparent` was ignored.
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    enable_propagation();
    if let Some(provider) = &metrics {
        opentelemetry::global::set_meter_provider(provider.clone());
        enable_metrics();
    }

    if let Some(protocol) = &config.unsupported_protocol {
        tracing::warn!(
            %protocol,
            "lesto exports OTLP over http/protobuf; OTEL_EXPORTER_OTLP_PROTOCOL is ignored"
        );
    }
    for problem in problems {
        tracing::error!(error = %problem, "an OTLP exporter could not be built");
    }
    tracing::debug!(
        service_name = %config.service_name,
        traces = traces.is_some(),
        logs = logs.is_some(),
        metrics = metrics.is_some(),
        "OTLP export configured"
    );
    Telemetry {
        traces,
        logs,
        metrics,
    }
}

/// Keep the telemetry stack out of the log pipeline it feeds.
///
/// Without this an export failure is logged, the log is exported, the export fails again: the
/// exporters and the HTTP client they use report through `tracing` like everything else. Their
/// records still reach the console, where they are harmless.
fn no_feedback() -> Targets {
    Targets::new()
        .with_default(LevelFilter::TRACE)
        .with_target("opentelemetry", LevelFilter::OFF)
        .with_target("opentelemetry_sdk", LevelFilter::OFF)
        .with_target("opentelemetry_otlp", LevelFilter::OFF)
        .with_target("opentelemetry-otlp", LevelFilter::OFF)
        .with_target("hyper", LevelFilter::OFF)
        .with_target("hyper_util", LevelFilter::OFF)
        .with_target("reqwest", LevelFilter::OFF)
        .with_target("h2", LevelFilter::OFF)
        .with_target("tower", LevelFilter::OFF)
}

fn unwrap_provider<P>(
    built: Result<P, opentelemetry_otlp::ExporterBuildError>,
    signal: &str,
    problems: &mut Vec<String>,
) -> Option<P> {
    match built {
        Ok(provider) => Some(provider),
        Err(error) => {
            problems.push(format!("{signal}: {error}"));
            None
        }
    }
}

/// The batching span provider, with the resource the collector groups telemetry by.
///
/// The endpoint, the headers and the timeout come from the environment, which the exporter
/// reads itself: `OTEL_EXPORTER_OTLP_ENDPOINT` (plus `/v1/traces`),
/// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (as given), `OTEL_EXPORTER_OTLP_HEADERS`.
fn tracer_provider(
    resource: &Resource,
) -> Result<SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()?;
    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource.clone())
        .build())
}

/// The batching log provider, reading `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` (or the generic
/// endpoint plus `/v1/logs`) the same way.
fn logger_provider(
    resource: &Resource,
) -> Result<SdkLoggerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .build()?;
    Ok(SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource.clone())
        .build())
}

/// The periodic metric provider (its own thread, every 60 s or `OTEL_METRIC_EXPORT_INTERVAL`),
/// reading `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` or the generic endpoint plus `/v1/metrics`.
fn meter_provider(
    resource: &Resource,
) -> Result<SdkMeterProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .build()?;
    Ok(SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter).build())
        .with_resource(resource.clone())
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vars: &[(&str, &str)]) -> Config {
        Config::read("fallback", |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn nothing_is_exported_without_an_endpoint() {
        let config = config(&[]);
        assert!(!config.exports());
        assert_eq!(config.service_name, "fallback");
        assert_eq!(config.filter, "info");
    }

    #[test]
    fn an_endpoint_turns_both_signals_on() {
        let config = config(&[
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "http://localhost:5080/api/default",
            ),
            ("OTEL_SERVICE_NAME", "notes"),
            ("RUST_LOG", "warn,lesto=info"),
        ]);
        assert!(config.exports_traces());
        assert!(config.exports_logs());
        assert_eq!(config.service_name, "notes");
        assert_eq!(config.filter, "warn,lesto=info");
        assert_eq!(config.unsupported_protocol, None);
    }

    #[test]
    fn a_signal_can_be_turned_off_on_its_own() {
        let logs_only = config(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
            ("OTEL_TRACES_EXPORTER", "none"),
        ]);
        assert!(!logs_only.exports_traces());
        assert!(logs_only.exports_logs());

        let traces_only = config(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
            ("OTEL_LOGS_EXPORTER", "none"),
        ]);
        assert!(traces_only.exports_traces());
        assert!(!traces_only.exports_logs());
    }

    #[test]
    fn a_logs_endpoint_alone_is_enough() {
        let config = config(&[(
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
            "http://collector/v1/logs",
        )]);
        assert!(config.exports_logs());
    }

    #[test]
    fn the_sdk_can_be_disabled_while_the_endpoint_stays() {
        let config = config(&[
            (
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://collector/v1/traces",
            ),
            ("OTEL_SDK_DISABLED", "true"),
        ]);
        assert!(config.has_endpoint);
        assert!(
            !config.exports(),
            "the console keeps working, the export does not"
        );
    }

    #[test]
    fn only_http_protobuf_is_supported() {
        let grpc = config(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4317"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        ]);
        assert_eq!(grpc.unsupported_protocol.as_deref(), Some("grpc"));
        let http = config(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")]);
        assert_eq!(http.unsupported_protocol, None);
    }

    #[test]
    fn empty_variables_are_treated_as_unset() {
        let config = config(&[("OTEL_SERVICE_NAME", "  "), ("RUST_LOG", "")]);
        assert_eq!(config.service_name, "fallback");
        assert_eq!(config.filter, "info");
    }
}

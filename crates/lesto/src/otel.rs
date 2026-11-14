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
//! | `LESTO_OTEL_SIGV4` | `on` signs every export with AWS SigV4 (region from the host, or `AWS_REGION`), `off` never; unset, an AWS endpoint is signed |
//! | `RUST_LOG` | the console and export filter, `info` by default |
//! | `LESTO_LOG` | the console's format, `text`, `json` or `off` ([`crate::log`]); not the export |
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
//! **AWS, with no collector.** An endpoint on AWS (`https://xray.<region>.amazonaws.com/v1/traces`,
//! `https://logs.<region>.amazonaws.com/v1/logs`, `https://monitoring.<region>.amazonaws.com/v1/metrics`)
//! is signed with SigV4, using the credentials in `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`
//! and `AWS_SESSION_TOKEN` — where Lambda puts those of the function's role. An `Authorization`
//! header of your own (an X-Ray API key in `OTEL_EXPORTER_OTLP_HEADERS`) is sent instead.
//! Without credentials that signal is not exported, and the reason is logged.
//!
//! The export is OTLP over **HTTP/protobuf**. `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` is not
//! honored: build your own exporters and subscriber then (chapter 15 of the tutorial) — lesto
//! stands aside as soon as a subscriber is installed.
//!
//! Call [`init`] by hand where `App::serve` is not the entry point (`lesto::lambda::serve`, a
//! test, a worker), and keep the returned [`Telemetry`] alive: dropping it flushes what is
//! still buffered. On AWS Lambda the process is frozen between invocations and never dropped,
//! so `lesto::lambda` calls [`flush`] after each one instead.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{ExporterBuildError, WithHttpConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::filter::FilterFn;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use crate::sigv4::{Credentials, Scope, Signer, SigningClient};

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
        // Enum and boolean values are case-insensitive (the specification's rule, which the OTLP
        // exporters follow too): `NONE`, `True` and `HTTP/PROTOBUF` mean what they say.
        let wanted =
            |name: &str| value(name).is_none_or(|v| !v.trim().eq_ignore_ascii_case("none"));
        Config {
            service_name: value("OTEL_SERVICE_NAME")
                .unwrap_or_else(|| default_service_name.to_string()),
            has_endpoint: value("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
                .or_else(|| value("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"))
                .or_else(|| value("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT"))
                .or_else(|| value("OTEL_EXPORTER_OTLP_ENDPOINT"))
                .is_some(),
            disabled: value("OTEL_SDK_DISABLED")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
            traces: wanted("OTEL_TRACES_EXPORTER"),
            logs: wanted("OTEL_LOGS_EXPORTER"),
            metrics: wanted("OTEL_METRICS_EXPORTER"),
            filter: value("RUST_LOG").unwrap_or_else(|| "info".to_string()),
            unsupported_protocol: protocol
                .filter(|p| !p.trim().to_ascii_lowercase().starts_with("http/protobuf")),
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
    /// Identifies this guard in [`INSTALLED`], so dropping an older guard does not unregister
    /// a newer one. `0` for a guard that owns nothing (never registered).
    id: u64,
    traces: Option<SdkTracerProvider>,
    logs: Option<SdkLoggerProvider>,
    metrics: Option<SdkMeterProvider>,
}

/// Clones of the providers of the live [`Telemetry`] (the SDK providers are reference counted),
/// so [`flush`] can reach them from code that does not hold the guard: the Lambda adapter.
#[derive(Clone)]
struct Providers {
    id: u64,
    traces: Option<SdkTracerProvider>,
    logs: Option<SdkLoggerProvider>,
    metrics: Option<SdkMeterProvider>,
}

static INSTALLED: std::sync::Mutex<Option<Providers>> = std::sync::Mutex::new(None);

fn installed() -> std::sync::MutexGuard<'static, Option<Providers>> {
    // A panic while holding the lock leaves an `Option` of clones: still consistent.
    INSTALLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Telemetry {
    /// A guard that owns nothing: the console subscriber, or nothing at all.
    fn none() -> Self {
        Telemetry {
            id: 0,
            traces: None,
            logs: None,
            metrics: None,
        }
    }

    /// A guard over providers the application built itself (the escape hatch: its own
    /// exporters, sampler or subscriber).
    ///
    /// Like the guard [`init`] returns, it flushes and shuts the providers down when dropped,
    /// and it makes them the ones [`flush`] flushes, which is what `lesto::lambda` calls after
    /// every invocation. Installing the subscriber, the propagator and the global meter
    /// provider stays the application's job ([`enable_propagation`], [`enable_metrics`]).
    pub fn from_providers(
        traces: Option<SdkTracerProvider>,
        logs: Option<SdkLoggerProvider>,
        metrics: Option<SdkMeterProvider>,
    ) -> Self {
        Self::register(traces, logs, metrics)
    }

    fn register(
        traces: Option<SdkTracerProvider>,
        logs: Option<SdkLoggerProvider>,
        metrics: Option<SdkMeterProvider>,
    ) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        if traces.is_none() && logs.is_none() && metrics.is_none() {
            return Self::none();
        }
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *installed() = Some(Providers {
            id,
            traces: traces.clone(),
            logs: logs.clone(),
            metrics: metrics.clone(),
        });
        Telemetry {
            id,
            traces,
            logs,
            metrics,
        }
    }

    /// Export what is buffered now, and wait for it: spans, metrics, then logs.
    ///
    /// Blocking: the batch processors export on threads of their own and this waits for them.
    /// From async code, call it through `tokio::task::spawn_blocking`.
    pub fn flush(&self) {
        flush_providers(
            self.traces.as_ref(),
            self.logs.as_ref(),
            self.metrics.as_ref(),
        );
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
        if self.id != 0 {
            let mut installed = installed();
            if installed.as_ref().is_some_and(|p| p.id == self.id) {
                *installed = None;
            }
        }
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

/// Export what the providers of the live [`Telemetry`] (from [`init`], [`init_named`],
/// [`Telemetry::from_providers`] or `App::serve`) have buffered, and wait for it. Nothing
/// happens when there is none.
///
/// For environments that stop the process between requests without a shutdown: the Lambda
/// adapter calls it after every invocation (see `lesto::lambda::Options::flush_telemetry`),
/// because Lambda freezes the environment as soon as the response is returned and the batch
/// processors' timers do not run while it is frozen. Blocking, like [`Telemetry::flush`].
pub fn flush() {
    let Some(providers) = installed().clone() else {
        return;
    };
    flush_providers(
        providers.traces.as_ref(),
        providers.logs.as_ref(),
        providers.metrics.as_ref(),
    );
}

/// [`flush`] on the blocking pool, skipped without a live [`Telemetry`] (no thread hop then).
#[cfg(feature = "lambda")]
pub(crate) async fn flush_async() {
    if installed().is_none() {
        return;
    }
    // A panic in an exporter is the exporter's; the response is already built.
    let _ = tokio::task::spawn_blocking(flush).await;
}

fn flush_providers(
    traces: Option<&SdkTracerProvider>,
    logs: Option<&SdkLoggerProvider>,
    metrics: Option<&SdkMeterProvider>,
) {
    // Spans and metrics first: a failure there is logged, and the log flush below takes it.
    if let Some(provider) = traces
        && let Err(error) = provider.force_flush()
    {
        tracing::warn!(%error, "the span exporter could not flush");
    }
    if let Some(provider) = metrics
        && let Err(error) = provider.force_flush()
    {
        tracing::warn!(%error, "the metric exporter could not flush");
    }
    if let Some(provider) = logs
        && let Err(error) = provider.force_flush()
    {
        // Printed, not logged: the log pipeline is the thing that failed.
        eprintln!("lesto: the log exporter could not flush: {error}");
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

/// Install the console subscriber ([`crate::log`]) and, when the environment points at a
/// collector, the OTLP export of spans, logs and metrics plus the W3C trace context propagator.
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
    install_with(config, |name| std::env::var(name).ok())
}

fn install_with(config: Config, env: impl Fn(&str) -> Option<String>) -> Telemetry {
    // Both providers are built before the subscriber, so a failure can be reported through it
    // afterwards rather than printed to a console nobody configured yet.
    let mut problems = Vec::new();
    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .build();
    let traces = config
        .exports_traces()
        .then(|| signing_client(Signal::Traces, &env).and_then(|c| tracer_provider(&resource, c)))
        .and_then(|built| unwrap_provider(built, "spans", &mut problems));
    let logs = config
        .exports_logs()
        .then(|| signing_client(Signal::Logs, &env).and_then(|c| logger_provider(&resource, c)))
        .and_then(|built| unwrap_provider(built, "logs", &mut problems));
    let metrics = config
        .exports_metrics()
        .then(|| signing_client(Signal::Metrics, &env).and_then(|c| meter_provider(&resource, c)))
        .and_then(|built| unwrap_provider(built, "metrics", &mut problems));

    // The console of `lesto::log`, in the format `LESTO_LOG` asks for (none for `off`).
    let log = crate::log::Config::from_env();
    let console =
        tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new(config.filter.clone()))
            .with(log.console())
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

    log.report();
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
    Telemetry::register(traces, logs, metrics)
}

/// Keep the telemetry stack out of the log pipeline it feeds.
///
/// Without this an export failure is logged, the log is exported, the export fails again: the
/// exporters and the HTTP client they use report through `tracing` like everything else. Their
/// records still reach the console, where they are harmless.
///
/// Matched per crate, not with `Targets`: its directives are string prefixes, so `opentelemetry`
/// also silenced `opentelemetry_example`, and `tower` a crate called `towerctl` — every log of an
/// application whose name happens to start like one of these.
fn no_feedback() -> FilterFn<fn(&tracing::Metadata<'_>) -> bool> {
    FilterFn::new(|metadata| !is_telemetry_target(metadata.target()))
}

/// The crates of the export path, as `tracing` targets (`crate` or `crate::module`).
const TELEMETRY_CRATES: &[&str] = &[
    "opentelemetry",
    "opentelemetry_sdk",
    "opentelemetry_otlp",
    "opentelemetry-otlp",
    "opentelemetry_http",
    "hyper",
    "hyper_util",
    "reqwest",
    "h2",
    "tower",
];

fn is_telemetry_target(target: &str) -> bool {
    let krate = target.split("::").next().unwrap_or(target);
    TELEMETRY_CRATES.contains(&krate)
}

fn unwrap_provider<P>(
    built: Result<P, ExporterBuildError>,
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

/// What differs between the three signals on the way out.
#[derive(Clone, Copy, Debug)]
enum Signal {
    Traces,
    Logs,
    Metrics,
}

impl Signal {
    /// The infix of the per-signal variables (`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`).
    fn var(self) -> &'static str {
        match self {
            Signal::Traces => "TRACES",
            Signal::Logs => "LOGS",
            Signal::Metrics => "METRICS",
        }
    }

    /// The AWS service of the signal's CloudWatch OTLP endpoint.
    fn aws_service(self) -> &'static str {
        match self {
            Signal::Traces => "xray",
            Signal::Logs => "logs",
            Signal::Metrics => "monitoring",
        }
    }
}

/// Whether the export of `signal` is signed with SigV4, and as what.
///
/// `Ok(None)`: sent as the exporter would on its own. `Err`: the signal should be signed and
/// cannot be, so it is not exported (an unsigned request to AWS is a 403 on every batch).
fn signing_plan(
    signal: Signal,
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<Signer>, String> {
    let value = |name: &str| get(name).filter(|v| !v.trim().is_empty());
    let forced = match value("LESTO_OTEL_SIGV4").map(|v| v.trim().to_ascii_lowercase()) {
        None => false,
        Some(v) if v == "on" || v == "true" => true,
        Some(v) if v == "off" || v == "false" => return Ok(None),
        Some(other) => {
            return Err(format!(
                "LESTO_OTEL_SIGV4 is `{other}`; it takes `on` or `off`"
            ));
        }
    };
    let endpoint = value(&format!("OTEL_EXPORTER_OTLP_{}_ENDPOINT", signal.var()))
        .or_else(|| value("OTEL_EXPORTER_OTLP_ENDPOINT"));
    let detected = endpoint
        .as_deref()
        .and_then(|endpoint| endpoint.trim().parse::<http::Uri>().ok())
        .and_then(|uri| uri.host().and_then(Scope::of_host));
    let scope = match detected {
        Some(scope) => scope,
        None if forced => Scope {
            service: signal.aws_service().to_string(),
            region: value("AWS_REGION")
                .or_else(|| value("AWS_DEFAULT_REGION"))
                .ok_or("LESTO_OTEL_SIGV4=on needs AWS_REGION when the endpoint is not an AWS host")?
                .trim()
                .to_string(),
        },
        None => return Ok(None),
    };
    // An API key of the application's (X-Ray's bearer tokens) is the authentication it chose.
    let headers = value(&format!("OTEL_EXPORTER_OTLP_{}_HEADERS", signal.var()))
        .or_else(|| value("OTEL_EXPORTER_OTLP_HEADERS"));
    let has_authorization = headers.is_some_and(|headers| {
        headers.split(',').any(|pair| {
            pair.split_once('=')
                .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("authorization"))
        })
    });
    if has_authorization && !forced {
        return Ok(None);
    }
    let credentials = Credentials::read(get).ok_or_else(|| {
        format!(
            "{} on AWS needs SigV4, and AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY are not set \
             (an API key in OTEL_EXPORTER_OTLP_HEADERS is the other way in)",
            scope.service
        )
    })?;
    Ok(Some(Signer { credentials, scope }))
}

/// The HTTP client of `signal`'s exporter when it signs, built as the exporter builds its own:
/// a blocking reqwest client with the `OTEL_EXPORTER_OTLP_*_TIMEOUT`, on a thread of its own
/// (a blocking client cannot be created inside an async runtime).
fn signing_client(
    signal: Signal,
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<SigningClient>, ExporterBuildError> {
    let Some(signer) =
        signing_plan(signal, get).map_err(ExporterBuildError::InvalidConfiguration)?
    else {
        return Ok(None);
    };
    let millis = |name: &str| get(name).and_then(|v| v.trim().parse::<u64>().ok());
    let timeout = millis(&format!("OTEL_EXPORTER_OTLP_{}_TIMEOUT", signal.var()))
        .or_else(|| millis("OTEL_EXPORTER_OTLP_TIMEOUT"))
        .map_or(
            opentelemetry_otlp::OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT,
            std::time::Duration::from_millis,
        );
    let inner = std::thread::spawn(move || {
        reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
    })
    .join()
    .map_err(|_| ExporterBuildError::InternalFailure("building the HTTP client panicked".into()))?
    .map_err(|error| ExporterBuildError::InternalFailure(error.to_string()))?;
    Ok(Some(SigningClient { inner, signer }))
}

/// The batching span provider, with the resource the collector groups telemetry by.
///
/// The endpoint, the headers and the timeout come from the environment, which the exporter
/// reads itself: `OTEL_EXPORTER_OTLP_ENDPOINT` (plus `/v1/traces`),
/// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (as given), `OTEL_EXPORTER_OTLP_HEADERS`.
fn tracer_provider(
    resource: &Resource,
    client: Option<SigningClient>,
) -> Result<SdkTracerProvider, ExporterBuildError> {
    let mut builder = opentelemetry_otlp::SpanExporter::builder().with_http();
    if let Some(client) = client {
        builder = builder.with_http_client(client);
    }
    let exporter = builder.build()?;
    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource.clone())
        .build())
}

/// The batching log provider, reading `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` (or the generic
/// endpoint plus `/v1/logs`) the same way.
fn logger_provider(
    resource: &Resource,
    client: Option<SigningClient>,
) -> Result<SdkLoggerProvider, ExporterBuildError> {
    let mut builder = opentelemetry_otlp::LogExporter::builder().with_http();
    if let Some(client) = client {
        builder = builder.with_http_client(client);
    }
    let exporter = builder.build()?;
    Ok(SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource.clone())
        .build())
}

/// The periodic metric provider (its own thread, every 60 s or `OTEL_METRIC_EXPORT_INTERVAL`),
/// reading `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` or the generic endpoint plus `/v1/metrics`.
fn meter_provider(
    resource: &Resource,
    client: Option<SigningClient>,
) -> Result<SdkMeterProvider, ExporterBuildError> {
    let mut builder = opentelemetry_otlp::MetricExporter::builder().with_http();
    if let Some(client) = client {
        builder = builder.with_http_client(client);
    }
    let exporter = builder.build()?;
    Ok(SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter).build())
        .with_resource(resource.clone())
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_filter_matches_whole_crate_names() {
        for silenced in [
            "opentelemetry",
            "opentelemetry_sdk::metrics",
            "hyper_util::client",
            "tower",
        ] {
            assert!(is_telemetry_target(silenced), "{silenced}");
        }
        for kept in [
            "opentelemetry_example",
            "towerctl::jobs",
            "h2o_api",
            "lesto::db::error",
            "app",
        ] {
            assert!(!is_telemetry_target(kept), "{kept}");
        }
    }

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
        let upper = config(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "HTTP/PROTOBUF")]);
        assert_eq!(
            upper.unsupported_protocol, None,
            "the exporters read it case-insensitively"
        );
    }

    #[test]
    fn enum_and_boolean_values_ignore_case() {
        let config = config(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
            ("OTEL_LOGS_EXPORTER", "None"),
            ("OTEL_SDK_DISABLED", "TRUE"),
        ]);
        assert!(!config.logs);
        assert!(config.disabled);
    }

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    const CREDENTIALS: [(&str, &str); 3] = [
        ("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE"),
        (
            "AWS_SECRET_ACCESS_KEY",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        ),
        ("AWS_SESSION_TOKEN", "token"),
    ];

    fn plan(signal: Signal, vars: &[(&str, &str)]) -> Result<Option<Scope>, String> {
        let mut all = CREDENTIALS.to_vec();
        all.extend_from_slice(vars);
        signing_plan(signal, &env(&all)).map(|signer| signer.map(|s| s.scope))
    }

    fn scope(service: &str, region: &str) -> Option<Scope> {
        Some(Scope {
            service: service.into(),
            region: region.into(),
        })
    }

    #[test]
    fn aws_endpoints_are_signed_others_are_not() {
        let xray = (
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "https://xray.eu-west-1.amazonaws.com/v1/traces",
        );
        assert_eq!(
            plan(Signal::Traces, &[xray]),
            Ok(scope("xray", "eu-west-1"))
        );
        // The per-signal endpoint is the traces' own: logs fall back to the generic one.
        assert_eq!(plan(Signal::Logs, &[xray]), Ok(None));
        assert_eq!(
            plan(
                Signal::Metrics,
                &[(
                    "OTEL_EXPORTER_OTLP_ENDPOINT",
                    "https://monitoring.us-east-1.amazonaws.com"
                )]
            ),
            Ok(scope("monitoring", "us-east-1"))
        );
        assert_eq!(
            plan(
                Signal::Traces,
                &[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4318")]
            ),
            Ok(None)
        );
        assert_eq!(plan(Signal::Traces, &[]), Ok(None));
    }

    #[test]
    fn sigv4_can_be_forced_or_turned_off() {
        let local = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318");
        assert_eq!(
            plan(
                Signal::Logs,
                &[
                    local,
                    ("LESTO_OTEL_SIGV4", "on"),
                    ("AWS_REGION", "eu-south-1")
                ]
            ),
            Ok(scope("logs", "eu-south-1")),
            "a proxy or a test double, signed as the signal's service"
        );
        assert_eq!(
            plan(
                Signal::Traces,
                &[
                    local,
                    ("LESTO_OTEL_SIGV4", "ON"),
                    ("AWS_DEFAULT_REGION", "us-west-2")
                ]
            ),
            Ok(scope("xray", "us-west-2"))
        );
        assert!(
            plan(Signal::Traces, &[local, ("LESTO_OTEL_SIGV4", "on")]).is_err(),
            "no region"
        );
        assert_eq!(
            plan(
                Signal::Traces,
                &[
                    (
                        "OTEL_EXPORTER_OTLP_ENDPOINT",
                        "https://xray.eu-west-1.amazonaws.com"
                    ),
                    ("LESTO_OTEL_SIGV4", "off")
                ]
            ),
            Ok(None)
        );
        assert!(plan(Signal::Traces, &[("LESTO_OTEL_SIGV4", "maybe")]).is_err());
    }

    #[test]
    fn an_api_key_wins_over_sigv4() {
        let xray = (
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "https://xray.eu-west-1.amazonaws.com",
        );
        assert_eq!(
            plan(
                Signal::Traces,
                &[
                    xray,
                    (
                        "OTEL_EXPORTER_OTLP_HEADERS",
                        "x-a=1, Authorization=Bearer%20key"
                    )
                ]
            ),
            Ok(None)
        );
        assert_eq!(
            plan(
                Signal::Traces,
                &[
                    xray,
                    (
                        "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
                        "authorization=Bearer%20key"
                    )
                ]
            ),
            Ok(scope("xray", "eu-west-1")),
            "the logs' key is not the traces'"
        );
    }

    #[test]
    fn an_aws_endpoint_without_credentials_is_not_exported() {
        let get = env(&[(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "https://xray.eu-west-1.amazonaws.com",
        )]);
        let error = signing_plan(Signal::Traces, &get).unwrap_err();
        assert!(error.contains("AWS_ACCESS_KEY_ID"), "{error}");
    }

    /// The request line, headers and body one request carried.
    type Captured = (String, Vec<(String, String)>, Vec<u8>);

    /// A one-request HTTP server on loopback: returns what it received.
    fn capture_one() -> (String, std::thread::JoinHandle<Captured>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/v1/traces", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
            }
            let length: usize = headers
                .iter()
                .find(|(n, _)| n == "content-length")
                .map_or(0, |(_, v)| v.parse().unwrap());
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut stream = stream;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .unwrap();
            (line, headers, body)
        });
        (address, handle)
    }

    #[test]
    fn the_signature_holds_for_what_goes_on_the_wire() {
        use opentelemetry::trace::{Tracer as _, TracerProvider as _};
        use opentelemetry_otlp::WithExportConfig;

        let (endpoint, server) = capture_one();
        let get = env(&[
            ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", &endpoint),
            ("LESTO_OTEL_SIGV4", "on"),
            ("AWS_REGION", "eu-west-1"),
            CREDENTIALS[0],
            CREDENTIALS[1],
            CREDENTIALS[2],
        ]);
        let client = signing_client(Signal::Traces, &get).unwrap().unwrap();
        let signer = client.signer.clone();
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .with_http_client(client)
            .build()
            .unwrap();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter)
            .build();
        provider.tracer("test").in_span("signed", |_| {});
        let (line, headers, body) = server.join().unwrap();
        let _ = provider.shutdown();

        let header = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("no {name} in {headers:?}"))
        };
        let authorization = header("authorization");
        let signed_names: Vec<String> = authorization
            .split("SignedHeaders=")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .split(';')
            .map(str::to_string)
            .collect();
        assert!(signed_names.contains(&"host".to_string()));
        assert!(signed_names.contains(&"x-amz-security-token".to_string()));
        // Recompute from the bytes the server received: a header the client changed after
        // signing (or a `host` spelled differently) breaks this, as it would at AWS.
        let received: Vec<(String, String)> = signed_names
            .iter()
            .map(|name| (name.clone(), header(name)))
            .collect();
        let path = line.split(' ').nth(1).unwrap();
        let payload_hash = {
            let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &body);
            digest
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let expected = signer.signature(
            "POST",
            path,
            "",
            &received,
            &payload_hash,
            &header("x-amz-date"),
        );
        assert_eq!(authorization, expected.authorization);
        assert!(authorization.contains("/eu-west-1/xray/aws4_request"));
    }

    #[test]
    fn empty_variables_are_treated_as_unset() {
        let config = config(&[("OTEL_SERVICE_NAME", "  "), ("RUST_LOG", "")]);
        assert_eq!(config.service_name, "fallback");
        assert_eq!(config.filter, "info");
    }
}

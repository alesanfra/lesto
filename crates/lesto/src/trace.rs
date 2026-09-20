//! Request spans, named and attributed after the OpenTelemetry semantic conventions for
//! [HTTP servers](https://opentelemetry.io/docs/specs/semconv/http/http-spans/).
//!
//! Every request handled by a router built with [`App::into_router`](crate::App::into_router)
//! runs inside a `tracing` span whose fields are the conventional attribute names
//! (`http.request.method`, `http.route`, `http.response.status_code`, ...) plus the `otel.*`
//! fields [`tracing-opentelemetry`] reads (`otel.name`, `otel.kind`, `otel.status_code`). With
//! that layer installed the span is exported as an OpenTelemetry `SERVER` span; with a plain
//! `tracing_subscriber::fmt` it is an ordinary span, and with no subscriber interested in it
//! the request runs on the inner future itself: what is left is the callsite check `tracing`
//! does to find that out ([`crate::layers::RequestSpanLayer`]).
//!
//! Spans are emitted at `INFO`. There is no level knob: filtering is the subscriber's job
//! (`RUST_LOG=lesto=info`), and `tracing` levels have to be compile-time constants.
//!
//! Configure with [`App::trace`](crate::App::trace):
//!
//! ```no_run
//! # use lesto::{App, Trace};
//! let app = App::<()>::new().trace(Trace::new().query(true).forwarded(true));
//! let quiet = App::<()>::new().trace(Trace::off());
//! ```
//!
//! With the `otel` feature the parent of the span is taken from the incoming request through
//! the global OpenTelemetry propagator, so a trace started upstream continues here. See
//! [`propagation`].
//!
//! [`tracing-opentelemetry`]: https://docs.rs/tracing-opentelemetry

use std::borrow::Cow;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, MatchedPath};
use axum::response::Response;
use http::{HeaderMap, Method, Version, header};
use tracing::Span;
use tracing::field::Empty;

/// What the request span records.
///
/// The defaults are the attributes that cannot carry application data: method, route, path,
/// scheme, status, protocol version and user agent. The two that can are opt-in.
#[derive(Clone, Copy, Debug)]
pub struct Trace {
    pub(crate) enabled: bool,
    query: bool,
    forwarded: bool,
}

impl Default for Trace {
    fn default() -> Self {
        Self {
            enabled: true,
            query: false,
            forwarded: false,
        }
    }
}

impl Trace {
    /// The default: request spans on, `url.query` off, forwarding headers not trusted.
    pub fn new() -> Self {
        Self::default()
    }

    /// No request spans at all.
    pub fn off() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    /// Record `url.query`.
    ///
    /// Off by default: a query string is application data and often holds a token, an email or
    /// a filter nobody meant to keep. The values of the keys the conventions call out
    /// (`sig`, `X-Amz-Signature`, `X-Amz-Credential`, `X-Amz-Security-Token`,
    /// `X-Goog-Signature`) are replaced by `REDACTED` even when this is on; everything else is
    /// recorded as it arrived, so turn it on only where the query string is safe to keep.
    pub fn query(mut self, record: bool) -> Self {
        self.query = record;
        self
    }

    /// Trust `X-Forwarded-For`, `X-Forwarded-Proto` and `X-Forwarded-Host` for
    /// `client.address`, `url.scheme` and `server.address`.
    ///
    /// Off by default: any client can send those headers, so they are only worth reading when
    /// a proxy you control rewrites them. Without it `client.address` is the peer address (when
    /// the app is served with `into_make_service_with_connect_info`) and the scheme is the one
    /// the process itself sees.
    pub fn forwarded(mut self, trust: bool) -> Self {
        self.forwarded = trust;
        self
    }
}

/// The `SERVER` span for `req`.
///
/// Every field is declared here, because `tracing` metadata is static: the ones that are only
/// known later (the status, the error) start [`Empty`] and are recorded by
/// [`record_response`]. `otel.name` carries the `{method} {route}` name, which a `tracing` span
/// name cannot (it must be a constant).
pub(crate) fn request_span<B>(req: &http::Request<B>, config: Trace) -> Span {
    let (method, method_original) = method_names(req.method());
    let span = tracing::info_span!(
        "http.server.request",
        otel.name = Empty,
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = method,
        http.request.method_original = Empty,
        http.route = Empty,
        http.response.status_code = Empty,
        url.path = Empty,
        url.query = Empty,
        url.scheme = Empty,
        server.address = Empty,
        server.port = Empty,
        client.address = Empty,
        network.peer.address = Empty,
        network.peer.port = Empty,
        network.protocol.version = Empty,
        user_agent.original = Empty,
        error.type = Empty,
    );
    // Nothing below is free (a `format!` for the name, a header walk); with no interested
    // subscriber the span is disabled and none of it has to happen.
    if span.is_disabled() {
        return span;
    }

    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    span.record(
        "otel.name",
        match route {
            Some(route) => Cow::Owned(format!("{method} {route}")),
            // No route: the request matched the fallback. `{method}` alone is what the
            // conventions ask for, and keeps the path out of the span name.
            None => Cow::Borrowed(method),
        }
        .as_ref(),
    );
    if let Some(route) = route {
        span.record("http.route", route);
    }
    if let Some(original) = method_original {
        span.record("http.request.method_original", original);
    }

    span.record("url.path", req.uri().path());
    span.record("url.scheme", scheme(req, config));
    if let Some(query) = req.uri().query().filter(|_| config.query) {
        span.record("url.query", redact_query(query).as_ref());
    }

    if let Some((host, port)) = server_address(req, config) {
        span.record("server.address", host);
        if let Some(port) = port {
            span.record("server.port", port);
        }
    }

    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);
    if let Some(peer) = peer {
        span.record("network.peer.address", tracing::field::display(peer.ip()));
        span.record("network.peer.port", peer.port());
    }
    let forwarded_for = config
        .forwarded
        .then(|| first_forwarded(req.headers(), "x-forwarded-for"))
        .flatten();
    if let Some(client) = forwarded_for {
        span.record("client.address", client);
    } else if let Some(peer) = peer {
        span.record("client.address", tracing::field::display(peer.ip()));
    }

    if let Some(version) = protocol_version(req.version()) {
        span.record("network.protocol.version", version);
    }
    if let Some(agent) = header_str(req.headers(), header::USER_AGENT) {
        span.record("user_agent.original", agent);
    }

    #[cfg(feature = "otel")]
    propagation::set_parent(&span, req.headers());

    span
}

/// Record the outcome on the span opened by [`request_span`].
///
/// A `5xx` is the server's own failure, so it sets `error.type` and the span status; a `4xx` is
/// the client's, and the conventions are explicit that a `SERVER` span must stay unset for it.
pub(crate) fn record_response(span: &Span, response: &Response) {
    let status = response.status();
    span.record("http.response.status_code", status.as_u16());
    if status.is_server_error() {
        span.record("error.type", status.as_str());
        span.record("otel.status_code", "ERROR");
    }
}

/// `http.request.method`, plus `http.request.method_original` when the method is not one of the
/// nine the conventions know.
///
/// The comparison is case-sensitive on purpose: `Get` is not `GET`, and mapping it to `GET`
/// would hide a misbehaving client.
fn method_names(method: &Method) -> (&'static str, Option<String>) {
    let known = match method.as_str() {
        "CONNECT" => "CONNECT",
        "DELETE" => "DELETE",
        "GET" => "GET",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "PATCH" => "PATCH",
        "POST" => "POST",
        "PUT" => "PUT",
        "TRACE" => "TRACE",
        other => return ("_OTHER", Some(other.to_owned())),
    };
    (known, None)
}

fn protocol_version(version: Version) -> Option<&'static str> {
    // `http::Version` is an opaque struct, so this is a chain of comparisons, not a `match`.
    for (known, name) in [
        (Version::HTTP_11, "1.1"),
        (Version::HTTP_2, "2"),
        (Version::HTTP_3, "3"),
        (Version::HTTP_10, "1.0"),
        (Version::HTTP_09, "0.9"),
    ] {
        if version == known {
            return Some(name);
        }
    }
    None
}

/// `url.scheme`: what the request line says, else what a trusted proxy says, else `http`.
fn scheme<B>(req: &http::Request<B>, config: Trace) -> &str {
    if let Some(scheme) = req.uri().scheme_str() {
        return scheme;
    }
    if let Some(proto) = config
        .forwarded
        .then(|| first_forwarded(req.headers(), "x-forwarded-proto"))
        .flatten()
    {
        return proto;
    }
    "http"
}

/// `server.address` and `server.port`, in the order the conventions prescribe: the forwarded
/// host (when trusted), then the `:authority` of HTTP/2 and HTTP/3, then `Host`.
fn server_address<B>(req: &http::Request<B>, config: Trace) -> Option<(&str, Option<u16>)> {
    let forwarded = config
        .forwarded
        .then(|| first_forwarded(req.headers(), "x-forwarded-host"))
        .flatten();
    let authority = req.uri().host().map(|host| match req.uri().port_u16() {
        Some(port) => (host, Some(port)),
        None => (host, None),
    });
    match (forwarded, authority) {
        (Some(host), _) => Some(split_host_port(host)),
        (None, Some(found)) => Some(found),
        (None, None) => header_str(req.headers(), header::HOST).map(split_host_port),
    }
}

/// `host`, `host:port` or `[::1]:port` → the host without brackets and the port.
fn split_host_port(value: &str) -> (&str, Option<u16>) {
    if let Some(rest) = value.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return (value, None);
        };
        return (host, tail.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match value.rsplit_once(':') {
        // A second colon means an unbracketed IPv6 address, which carries no port.
        Some((host, port)) if !host.contains(':') => match port.parse() {
            Ok(port) => (host, Some(port)),
            Err(_) => (value, None),
        },
        _ => (value, None),
    }
}

/// The first entry of a comma-separated forwarding header (`X-Forwarded-For: client, proxy`).
fn first_forwarded<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    let value = headers.get(name)?.to_str().ok()?;
    let first = value.split(',').next()?.trim();
    (!first.is_empty()).then_some(first)
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name)?.to_str().ok()
}

/// Query-string keys whose value the conventions say to replace by `REDACTED`.
const REDACTED_QUERY_KEYS: [&str; 5] = [
    "sig",
    "X-Amz-Signature",
    "X-Amz-Credential",
    "X-Amz-Security-Token",
    "X-Goog-Signature",
];

/// Replace the values of [`REDACTED_QUERY_KEYS`], keeping their keys and the rest of the query
/// string untouched. Borrows when there is nothing to redact, which is the common case.
fn redact_query(query: &str) -> Cow<'_, str> {
    let is_secret = |pair: &str| {
        let key = pair.split('=').next().unwrap_or(pair);
        REDACTED_QUERY_KEYS
            .iter()
            .any(|k| k.eq_ignore_ascii_case(key))
    };
    if !query.split('&').any(is_secret) {
        return Cow::Borrowed(query);
    }
    let mut out = String::with_capacity(query.len());
    for (i, pair) in query.split('&').enumerate() {
        if i > 0 {
            out.push('&');
        }
        if is_secret(pair) {
            let key = pair.split('=').next().unwrap_or(pair);
            out.push_str(key);
            out.push_str("=REDACTED");
        } else {
            out.push_str(pair);
        }
    }
    Cow::Owned(out)
}

/// Trace context propagation (feature `otel`).
///
/// The parent of the request span is whatever the global OpenTelemetry propagator finds in the
/// request headers, so a `traceparent` sent by the caller continues the same trace here.
///
/// **A propagator has to be installed**, once, next to the tracer — the OpenTelemetry API ships
/// a no-op by default and lesto does not choose for you (W3C, B3 and the others live in
/// different crates) — and lesto has to be told, so that an application exporting nothing pays
/// nothing:
///
/// ```ignore
/// opentelemetry::global::set_text_map_propagator(
///     opentelemetry_sdk::propagation::TraceContextPropagator::new(),
/// );
/// lesto::otel::enable_propagation();
/// ```
///
/// [`otel::init`](crate::otel::init) and `App::serve` do both by themselves.
///
/// Attaching the parent also needs `tracing_opentelemetry::layer()` in the subscriber; without
/// it the call is a no-op and the span is simply a root.
#[cfg(feature = "otel")]
pub mod propagation {
    use std::sync::atomic::{AtomicBool, Ordering};

    use http::HeaderMap;
    use opentelemetry::propagation::Extractor;
    use tracing::Span;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    /// Adapts request headers to the OpenTelemetry `Extractor` the propagators take.
    struct Headers<'a>(&'a HeaderMap);

    impl Extractor for Headers<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key).and_then(|value| value.to_str().ok())
        }

        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(http::HeaderName::as_str).collect()
        }
    }

    /// Whether a propagator worth asking has been installed. See
    /// [`otel::enable_propagation`](crate::otel::enable_propagation).
    pub(crate) static ACTIVE: AtomicBool = AtomicBool::new(false);

    /// Make the context carried by `headers` the parent of `span`.
    ///
    /// Nothing at all until [`otel::enable_propagation`](crate::otel::enable_propagation) has
    /// run: the global propagator defaults to a no-op, and asking it on every request buys a
    /// lookup and a header walk for an answer that is always "no parent".
    pub fn set_parent(span: &Span, headers: &HeaderMap) {
        if !ACTIVE.load(Ordering::Relaxed) {
            return;
        }
        let context =
            opentelemetry::global::get_text_map_propagator(|p| p.extract(&Headers(headers)));
        // `Err` means the subscriber has no `tracing_opentelemetry::layer()`, so there is no
        // OpenTelemetry span to reparent: the request is still traced, as a root.
        let _ = span.set_parent(context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_unknown_methods() {
        assert_eq!(method_names(&Method::GET), ("GET", None));
        let query = Method::from_bytes(b"QUERY").unwrap();
        assert_eq!(
            method_names(&query),
            ("_OTHER", Some("QUERY".to_string())),
            "an extension method is `_OTHER` plus the original"
        );
    }

    #[test]
    fn host_and_port() {
        assert_eq!(split_host_port("example.com"), ("example.com", None));
        assert_eq!(
            split_host_port("example.com:8080"),
            ("example.com", Some(8080))
        );
        assert_eq!(split_host_port("[::1]:8080"), ("::1", Some(8080)));
        assert_eq!(split_host_port("[::1]"), ("::1", None));
        assert_eq!(
            split_host_port("::1"),
            ("::1", None),
            "unbracketed IPv6 has no port"
        );
    }

    #[test]
    fn secrets_are_redacted_in_the_query_string() {
        assert!(matches!(redact_query("a=1&b=2"), Cow::Borrowed(_)));
        assert_eq!(
            redact_query("q=OpenTelemetry&sig=abc&X-Amz-Signature=def"),
            "q=OpenTelemetry&sig=REDACTED&X-Amz-Signature=REDACTED"
        );
    }
}

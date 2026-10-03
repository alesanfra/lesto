//! Logs on standard output, as text or JSON (feature `log`, on by default).
//!
//! [`App::serve`](crate::App::serve) installs a subscriber by itself when the application has
//! none: every `tracing` event at `info` and above, plus one line per request, printed on
//! stdout. Nothing to write in `main`; the environment picks the format:
//!
//! ```sh
//! cargo run                              # text
//! LESTO_LOG=json cargo run               # one JSON object per line
//! LESTO_LOG=off cargo run                # nothing
//! RUST_LOG=info,lesto=debug cargo run    # the filter, `info` by default
//! ```
//!
//! | variable | meaning |
//! |---|---|
//! | `LESTO_LOG` | `text` (the default), `json` or `off` |
//! | `RUST_LOG` | which events are printed, `info` by default |
//! | `NO_COLOR` | no ANSI colors in text, even on a terminal (they are off when stdout is not one) |
//! | `AWS_LAMBDA_LOG_FORMAT` | `JSON` (the function's log format set to JSON) makes `json` the default |
//!
//! Text is meant for a terminal. A request is shown as its method and path, a store
//! transaction as its method, and the line per request (the access log) as method, path, status
//! and duration:
//!
//! ```text
//! 2026-10-03T09:15:00.120456Z  INFO lesto::app: listening on http://127.0.0.1:8000
//! 2026-10-03T09:15:01.004311Z  INFO POST /notes 201 8.9ms
//! 2026-10-03T09:15:02.381090Z ERROR GET /notes/7 lesto::layers: handler panicked panic=boom
//! 2026-10-03T09:15:02.381233Z ERROR GET /notes/7 500 412µs
//! 2026-10-03T09:15:03.017502Z ERROR POST /notes NoteStore::create lesto::db::error: store method failed error="UNIQUE constraint failed: notes.title"
//! ```
//!
//! JSON is meant for a log collector: one flat object per line, `timestamp`, `level`, `target`
//! and `message` first, then the request (`http.request.method`, `http.route`, `url.path`), the
//! store method (`store`), the fields of any other span, and the event's own fields:
//!
//! ```text
//! {"timestamp":"2026-10-03T09:15:01.004311Z","level":"INFO","target":"lesto::access","message":"POST /notes 201","http.request.method":"POST","http.route":"/notes","url.path":"/notes","http.response.status_code":201,"duration_ms":8.9}
//! ```
//!
//! The access log is a `tracing` event like any other, with the target `lesto::access` (`INFO`,
//! `ERROR` for a `5xx`): `RUST_LOG=info,lesto::access=off` turns it off. The query string is
//! never in it.
//!
//! **lesto stands aside when the application installs a subscriber of its own** before
//! `serve`, and [`Console`] is the layer this module installs, so the format can be kept in a
//! subscriber built by hand. With the `otel` feature and an OTLP endpoint, `serve` installs the
//! same console next to the export. Where `serve` is not the entry point (a worker, a test,
//! `lesto::lambda::serve` calls it by itself), call [`init`].

use std::fmt::Write as _;
use std::io::{IsTerminal, Write as _};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

/// The target of the line written per request, by
/// [`RequestSpanLayer`](crate::layers::RequestSpanLayer).
pub(crate) const ACCESS_TARGET: &str = "lesto::access";

/// How [`Console`] writes a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// For a terminal: timestamp, level, request, target, message, `key=value` fields.
    Text,
    /// For a log collector: one flat JSON object per line.
    Json,
}

/// The variables this module reads, read once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    /// `None`: `LESTO_LOG=off`.
    pub(crate) format: Option<Format>,
    /// `RUST_LOG`, `info` by default.
    pub(crate) filter: String,
    /// `NO_COLOR` is unset.
    color: bool,
    /// A `LESTO_LOG` that is none of the three, reported once the subscriber is installed.
    unknown: Option<String>,
}

impl Config {
    pub(crate) fn from_env() -> Self {
        Self::read(|name| std::env::var(name).ok())
    }

    /// [`from_env`](Self::from_env) against any source of variables: the process environment
    /// must not be written to, so tests pass their own.
    fn read(get: impl Fn(&str) -> Option<String>) -> Self {
        let value = |name: &str| {
            get(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let lambda_json =
            value("AWS_LAMBDA_LOG_FORMAT").is_some_and(|v| v.eq_ignore_ascii_case("json"));
        let default = if lambda_json {
            Format::Json
        } else {
            Format::Text
        };
        let (format, unknown) = match value("LESTO_LOG") {
            None => (Some(default), None),
            Some(v) if v.eq_ignore_ascii_case("text") => (Some(Format::Text), None),
            Some(v) if v.eq_ignore_ascii_case("json") => (Some(Format::Json), None),
            Some(v) if v.eq_ignore_ascii_case("off") => (None, None),
            Some(v) => (Some(default), Some(v)),
        };
        Config {
            format,
            filter: value("RUST_LOG").unwrap_or_else(|| "info".to_string()),
            color: value("NO_COLOR").is_none(),
            unknown,
        }
    }

    /// The console layer this configuration asks for, `None` for `LESTO_LOG=off`.
    pub(crate) fn console(&self) -> Option<Console> {
        let format = self.format?;
        let ansi = self.color && format == Format::Text && std::io::stdout().is_terminal();
        Some(Console::new(format).with_ansi(ansi))
    }

    /// Say what was wrong with the configuration, through the subscriber just installed.
    pub(crate) fn report(&self) {
        if let Some(value) = &self.unknown {
            tracing::warn!(%value, "LESTO_LOG is text, json or off; using the default format");
        }
    }
}

/// Install a subscriber printing to stdout as `LESTO_LOG` and `RUST_LOG` say, unless the
/// application already installed one. `true` when this call installed it.
///
/// [`App::serve`](crate::App::serve) and `lesto::lambda::serve` call it by themselves; call it
/// where neither is the entry point.
pub fn init() -> bool {
    if tracing::dispatcher::has_been_set() {
        return false;
    }
    let config = Config::from_env();
    let Some(console) = config.console() else {
        return false;
    };
    let installed = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(&config.filter))
        .with(console)
        .try_init()
        .is_ok();
    if installed {
        config.report();
    }
    installed
}

/// The [`Layer`] that writes one line per `tracing` event, in a [`Format`].
///
/// [`init`] installs it; it is public for a subscriber built by hand, which then keeps lesto's
/// format:
///
/// ```no_run
/// use tracing_subscriber::layer::SubscriberExt;
/// use tracing_subscriber::util::SubscriberInitExt;
///
/// tracing_subscriber::registry()
///     .with(tracing_subscriber::EnvFilter::new("info"))
///     .with(lesto::log::Console::new(lesto::log::Format::Json))
///     .init();
/// ```
///
/// A write that fails (stdout closed) is dropped: logging never fails a request.
pub struct Console<W = fn() -> std::io::Stdout> {
    format: Format,
    ansi: bool,
    writer: W,
}

impl Console {
    /// Write `format` to stdout, with no colors.
    pub fn new(format: Format) -> Self {
        Console {
            format,
            ansi: false,
            writer: std::io::stdout,
        }
    }
}

impl<W> Console<W> {
    /// Write somewhere else than stdout: anything `tracing_subscriber` can make a writer of
    /// (`std::io::stderr`, a file behind a `Mutex`, a test buffer).
    pub fn with_writer<W2>(self, writer: W2) -> Console<W2>
    where
        W2: for<'a> MakeWriter<'a>,
    {
        Console {
            format: self.format,
            ansi: self.ansi,
            writer,
        }
    }

    /// Color the level and dim the timestamp and target with ANSI escapes (text only).
    pub fn with_ansi(mut self, ansi: bool) -> Self {
        self.ansi = ansi;
        self
    }
}

impl<W> std::fmt::Debug for Console<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console")
            .field("format", &self.format)
            .field("ansi", &self.ansi)
            .finish_non_exhaustive()
    }
}

impl<S, W> Layer<S> for Console<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'a> MakeWriter<'a> + 'static,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        span.extensions_mut().insert(fields);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        if let Some(fields) = span.extensions_mut().get_mut::<Fields>() {
            values.record(fields);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut line = match self.format {
            Format::Text => self.text(event.metadata(), &fields, &ctx, event),
            Format::Json => json(event.metadata(), fields, &ctx, event),
        };
        line.push('\n');
        let mut writer = self.writer.make_writer_for(event.metadata());
        let _ = writer.write_all(line.as_bytes());
    }
}

// ---- what a span or an event recorded -----------------------------------------------------

/// A recorded value, typed so the JSON keeps numbers and booleans.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Str(String),
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
}

impl Value {
    fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    fn json(self) -> serde_json::Value {
        match self {
            Value::Str(s) => s.into(),
            Value::I64(n) => n.into(),
            Value::U64(n) => n.into(),
            Value::F64(n) => n.into(),
            Value::Bool(b) => b.into(),
        }
    }

    /// `key=value`, logfmt style: a string is quoted only when it would not read back as one
    /// value (empty, spaces, quotes, `=`, control characters).
    fn text(&self, out: &mut String) {
        let _ = match self {
            Value::Str(s) if needs_quotes(s) => write!(out, "{s:?}"),
            Value::Str(s) => write!(out, "{s}"),
            Value::I64(n) => write!(out, "{n}"),
            Value::U64(n) => write!(out, "{n}"),
            Value::F64(n) => write!(out, "{n}"),
            Value::Bool(b) => write!(out, "{b}"),
        };
    }
}

fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '=')
}

/// The fields of a span (kept in its extensions, updated by `record`) or of an event (with
/// the message apart). In declaration order; a field recorded twice keeps its place.
#[derive(Debug, Default)]
struct Fields {
    message: Option<String>,
    values: Vec<(&'static str, Value)>,
}

impl Fields {
    fn get(&self, name: &str) -> Option<&Value> {
        self.values.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    fn str(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(Value::as_str)
    }

    fn set(&mut self, field: &Field, value: Value) {
        let name = field.name();
        if name == "message"
            && let Value::Str(message) = value
        {
            self.message = Some(message);
            return;
        }
        match self.values.iter_mut().find(|(n, _)| *n == name) {
            Some((_, old)) => *old = value,
            None => self.values.push((name, value)),
        }
    }
}

impl Visit for Fields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.set(field, Value::F64(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.set(field, Value::I64(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.set(field, Value::U64(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.set(field, Value::Bool(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.set(field, Value::Str(value.to_owned()));
    }

    /// The error and its sources, `outer: inner: root`, the way `anyhow` prints a chain. A
    /// source whose message the text already holds is skipped: many errors (sqlx's among them)
    /// repeat their source in their own message.
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        let mut text = value.to_string();
        let mut source = value.source();
        while let Some(cause) = source {
            let cause_text = cause.to_string();
            if !text.contains(&cause_text) {
                let _ = write!(text, ": {cause_text}");
            }
            source = cause.source();
        }
        self.set(field, Value::Str(text));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.set(field, Value::Str(format!("{value:?}")));
    }
}

/// The spans lesto opens, which both formats show by what they mean rather than by their
/// fields.
enum SpanKind {
    /// `http.server.request` (`crate::trace`).
    Request,
    /// `db.client.operation` (`crate::db::trace`).
    Store,
    Other,
}

fn span_kind(metadata: &Metadata<'_>) -> SpanKind {
    if !metadata.target().starts_with("lesto") {
        return SpanKind::Other;
    }
    match metadata.name() {
        "http.server.request" => SpanKind::Request,
        "db.client.operation" => SpanKind::Store,
        _ => SpanKind::Other,
    }
}

fn timestamp(out: &mut String) {
    let _ = SystemTime.format_time(&mut Writer::new(out));
}

// ---- text ---------------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";

impl<W> Console<W> {
    fn text<S>(
        &self,
        metadata: &Metadata<'_>,
        fields: &Fields,
        ctx: &Context<'_, S>,
        event: &Event<'_>,
    ) -> String
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let mut line = String::with_capacity(160);
        self.dim(&mut line, timestamp);
        line.push(' ');
        self.level(&mut line, *metadata.level());
        line.push(' ');

        if metadata.target() == ACCESS_TARGET {
            access_text(&mut line, fields);
            return line;
        }

        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                let extensions = span.extensions();
                let Some(recorded) = extensions.get::<Fields>() else {
                    continue;
                };
                match span_kind(span.metadata()) {
                    SpanKind::Request => {
                        let method = recorded
                            .str("http.request.method_original")
                            .or_else(|| recorded.str("http.request.method"))
                            .unwrap_or("-");
                        let path = recorded.str("url.path").unwrap_or("-");
                        let _ = write!(line, "{method} {path} ");
                    }
                    SpanKind::Store => {
                        if let Some(name) = recorded.str("otel.name") {
                            let _ = write!(line, "{name} ");
                        }
                    }
                    SpanKind::Other => {
                        line.push_str(span.name());
                        if !recorded.values.is_empty() {
                            line.push('{');
                            push_pairs(&mut line, &recorded.values);
                            line.push('}');
                        }
                        line.push(' ');
                    }
                }
            }
        }

        self.dim(&mut line, |out| {
            out.push_str(metadata.target());
            out.push(':');
        });
        if let Some(message) = &fields.message {
            line.push(' ');
            line.push_str(message);
        }
        if !fields.values.is_empty() {
            line.push(' ');
            push_pairs(&mut line, &fields.values);
        }
        line
    }

    fn dim(&self, out: &mut String, write: impl FnOnce(&mut String)) {
        if self.ansi {
            out.push_str(DIM);
        }
        write(out);
        if self.ansi {
            out.push_str(RESET);
        }
    }

    fn level(&self, out: &mut String, level: Level) {
        let color = match level {
            Level::ERROR => "\x1b[31m",
            Level::WARN => "\x1b[33m",
            Level::INFO => "\x1b[32m",
            Level::DEBUG => "\x1b[34m",
            Level::TRACE => "\x1b[35m",
        };
        if self.ansi {
            out.push_str(color);
        }
        let _ = write!(out, "{:>5}", level.as_str());
        if self.ansi {
            out.push_str(RESET);
        }
    }
}

fn push_pairs(out: &mut String, values: &[(&'static str, Value)]) {
    for (i, (name, value)) in values.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(name);
        out.push('=');
        value.text(out);
    }
}

/// `GET /notes/7 200 3.2ms`: what the access log says, in the order a person reads it.
fn access_text(out: &mut String, fields: &Fields) {
    let method = fields.str("http.request.method").unwrap_or("-");
    let path = fields.str("url.path").unwrap_or("-");
    let _ = write!(out, "{method} {path}");
    if let Some(status) = fields.get("http.response.status_code") {
        out.push(' ');
        status.text(out);
    }
    if let Some(Value::F64(ms)) = fields.get("duration_ms") {
        out.push(' ');
        push_duration(out, *ms);
    }
}

fn push_duration(out: &mut String, ms: f64) {
    let _ = if ms < 1.0 {
        write!(out, "{}µs", (ms * 1000.0).round())
    } else if ms < 1000.0 {
        write!(out, "{ms:.1}ms")
    } else {
        write!(out, "{:.2}s", ms / 1000.0)
    };
}

// ---- JSON ---------------------------------------------------------------------------------

fn json<S>(
    metadata: &Metadata<'_>,
    fields: Fields,
    ctx: &Context<'_, S>,
    event: &Event<'_>,
) -> String
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let mut object = serde_json::Map::new();
    let mut time = String::with_capacity(32);
    timestamp(&mut time);
    object.insert("timestamp".into(), time.into());
    object.insert("level".into(), metadata.level().as_str().into());
    object.insert("target".into(), metadata.target().into());
    if let Some(message) = fields.message {
        object.insert("message".into(), message.into());
    }

    if let Some(scope) = ctx.event_scope(event) {
        for span in scope.from_root() {
            let extensions = span.extensions();
            let Some(recorded) = extensions.get::<Fields>() else {
                continue;
            };
            match span_kind(span.metadata()) {
                SpanKind::Request => {
                    // What identifies the request; the rest of the span (user agent, peer,
                    // protocol) is for the trace, not for every line.
                    for name in ["http.request.method", "http.route", "url.path"] {
                        if let Some(value) = recorded.get(name) {
                            object.insert(name.into(), value.clone().json());
                        }
                    }
                }
                SpanKind::Store => {
                    if let Some(name) = recorded.str("otel.name") {
                        object.insert("store".into(), name.into());
                    }
                }
                SpanKind::Other => {
                    for (name, value) in &recorded.values {
                        object.insert((*name).into(), value.clone().json());
                    }
                }
            }
        }
    }

    for (name, value) in fields.values {
        object.insert(name.into(), value.json());
    }
    serde_json::to_string(&object).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vars: &[(&str, &str)]) -> Config {
        Config::read(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn text_at_info_by_default() {
        let config = config(&[]);
        assert_eq!(config.format, Some(Format::Text));
        assert_eq!(config.filter, "info");
        assert!(config.color);
    }

    #[test]
    fn the_format_is_chosen_by_lesto_log() {
        assert_eq!(config(&[("LESTO_LOG", "json")]).format, Some(Format::Json));
        assert_eq!(
            config(&[("LESTO_LOG", " JSON ")]).format,
            Some(Format::Json)
        );
        assert_eq!(config(&[("LESTO_LOG", "off")]).format, None);
        assert_eq!(config(&[("LESTO_LOG", "")]).format, Some(Format::Text));
        let typo = config(&[("LESTO_LOG", "jsno")]);
        assert_eq!(typo.format, Some(Format::Text));
        assert_eq!(typo.unknown.as_deref(), Some("jsno"));
    }

    #[test]
    fn lambda_json_logging_makes_json_the_default() {
        let lambda = [("AWS_LAMBDA_LOG_FORMAT", "JSON")];
        assert_eq!(config(&lambda).format, Some(Format::Json));
        let explicit = [("AWS_LAMBDA_LOG_FORMAT", "JSON"), ("LESTO_LOG", "text")];
        assert_eq!(config(&explicit).format, Some(Format::Text));
        assert_eq!(
            config(&[("AWS_LAMBDA_LOG_FORMAT", "Text")]).format,
            Some(Format::Text)
        );
    }

    #[test]
    fn no_color_and_rust_log() {
        let config = config(&[("NO_COLOR", "1"), ("RUST_LOG", "warn,lesto=debug")]);
        assert!(!config.color);
        assert_eq!(config.filter, "warn,lesto=debug");
    }

    #[test]
    fn strings_are_quoted_only_when_ambiguous() {
        let text = |value: Value| {
            let mut out = String::new();
            value.text(&mut out);
            out
        };
        assert_eq!(text(Value::Str("boom".into())), "boom");
        assert_eq!(text(Value::Str("two words".into())), "\"two words\"");
        assert_eq!(text(Value::Str(String::new())), "\"\"");
        assert_eq!(text(Value::Str("a=b".into())), "\"a=b\"");
        assert_eq!(text(Value::U64(200)), "200");
    }

    #[test]
    fn durations_read_like_durations() {
        let shown = |ms: f64| {
            let mut out = String::new();
            push_duration(&mut out, ms);
            out
        };
        assert_eq!(shown(0.412), "412µs");
        assert_eq!(shown(3.21), "3.2ms");
        assert_eq!(shown(1250.0), "1.25s");
    }
}

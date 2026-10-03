# Changelog

All notable changes to lesto are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/) (see "Versioning" in the README).

## [Unreleased]

## [0.2.0] - 2026-10-03

### Added

- Feature `log`, on by default: `App::serve` and `lesto::lambda::serve` print on stdout when the
  application installed no `tracing` subscriber — a line when the server starts listening, one
  per request (`GET /notes/7 200 412µs`, target `lesto::access`, the message `GET /notes/7 200`
  so a log backend shows it) and every event at `info` and above. `LESTO_LOG=text|json|off`
  picks the format (JSON by default when `AWS_LAMBDA_LOG_FORMAT=JSON`), `RUST_LOG` filters.
  `lesto::log::Console` is the layer, for a subscriber built by hand; `lesto::log::init()`
  installs it where `serve` is not the entry point.

### Changed

- An application with no `tracing` subscriber of its own now prints logs; `LESTO_LOG=off` or a
  subscriber installed before `serve` restores the silence. Applications with their own
  subscriber now also receive the `lesto::access` event once per request.
- Feature `otel` on OpenTelemetry 0.33 (`tracing-opentelemetry` 0.34): OTLP/HTTP exports are
  retried on `429`/`502`/`503`/`504` (3 retries, `Retry-After` honored), and a span or log record
  queued right before shutdown is no longer dropped by the final flush. No API change in lesto.
- The `OTEL_*` values lesto reads itself ignore case, as the specification says: `NONE` in
  `OTEL_LOGS_EXPORTER`, `TRUE` in `OTEL_SDK_DISABLED`, `HTTP/PROTOBUF` in
  `OTEL_EXPORTER_OTLP_PROTOCOL` (which was reported as unsupported).
- Dependency lower bounds raised to versions lesto actually builds with (`tokio` 1.46, `tracing`
  0.1.43, `serde` 1.0.228, `tower-http` 0.6.8, `axum` 0.8.2, ...), checked with
  `-Z direct-minimal-versions`; the old ones (`tokio = "1"`, `tracing = "0.1"`) let Cargo pick
  releases lesto cannot compile against. CI's MSRV job now runs the same check.
- With `otel`, the console next to the OTLP export uses the same format (it was
  `tracing_subscriber::fmt`'s), and the access log is exported as a log record per request.

## [0.1.1] - 2026-10-03

### Added

- The tutorial is published at <https://alesanfra.github.io/lesto/>, and the crates' `homepage`
  points there.
- A `NOTICE` file naming the copyright holder, shipped in every crate.

### Changed

- A shorter README; the route attribute options moved to the tutorial's appendix A, the
  performance measurements to a new appendix E.
- The tutorial depends on the published crates (`lesto = "0.1"`, `cargo install lesto-cli`).
- The crate documentation links the tutorial.

### Fixed

- Documentation that described what lesto no longer does: a FastAPI error format (removed before
  0.1.0), security extractors that never verify a token (`Jwt` verifies OpenID Connect tokens),
  MCP serving tools only, examples with `#[tokio::main]` and `#[crate::get]`.

## [0.1.0] - 2026-10-03

First release: `lesto`, `lesto-macros`, `lesto-cli`.

### Added

- Route attributes (`#[lesto::get]` and friends), `routes!`, `App` with OpenAPI 3.1 derived from
  handler types, Scalar at `/docs` and Swagger UI at `/swagger`.
- Validating extractors `Json`, `Query`, `Path` (garde); RFC 9457 errors (`HttpError`, `Problem`).
- Security extractors `Bearer`, `Basic`, `ApiKey`, documented as OpenAPI security schemes.
- `#[lesto::model]` (serde, schemars and garde derives through lesto, garde's derive vendored)
  and `#[lesto::views]` / `model(views(..))` for create/update views of one model.
- `#[lesto::main]` and `#[lesto::test]`: tokio's, through `lesto::tokio`.
- Feature `db`: sqlx stores with principals, permissions and one transaction per method.
- Feature `lambda`: AWS Lambda adapter; feature `otel`: OTLP traces and logs from the environment.
- Feature `oidc`: bearer JWTs verified against an OpenID Connect provider (`Oidc::discover`,
  `Oidc::from_env`, `Jwt<C>`, `StandardClaims`), `App::oidc` and `App::protect` for a group of
  routes, with the `openIdConnect` security scheme in the OpenAPI document.
- Feature `mcp`: routes marked `mcp = "tool"`, `"resource"` or `"prompt"` served to AI agents
  over the Model Context Protocol at `/mcp` (`App::mcp`), 2026-07-28 and the 2025 legacy
  versions, stateless. A tool call is an in-process request to the route, so validation, auth
  and layers are the route's own; `lesto::mcp::Prompt` is the response of a prompt route.
- Features `email` and `url` (default on) for the garde rules of the same name.
- JSON keys keep declaration order in the OpenAPI document and in response bodies.
- `Created<T>`, `Accepted<T>`, `NoContent`: the success status in the return type.
- `lesto dev`, `lesto run`, `lesto openapi`.
- `lesto::db::NotFoundExt::or_not_found`: a bare `sqlx::Error::RowNotFound` answers 500, the
  lookups that mean 404 say so.

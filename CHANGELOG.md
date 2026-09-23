# Changelog

All notable changes to lesto are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/) (see "Versioning" in the README).

## [Unreleased]

First release, 0.1.0: `lesto`, `lesto-macros`, `lesto-cli`.

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
- Features `email` and `url` (default on) for the garde rules of the same name.
- JSON keys keep declaration order in the OpenAPI document and in response bodies.
- `Created<T>`, `Accepted<T>`, `NoContent`: the success status in the return type.
- `lesto dev`, `lesto run`, `lesto openapi`.
- `lesto::db::NotFoundExt::or_not_found`: a bare `sqlx::Error::RowNotFound` answers 500, the
  lookups that mean 404 say so.

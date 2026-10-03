# lesto

[![crates.io](https://img.shields.io/crates/v/lesto.svg)](https://crates.io/crates/lesto)
[![docs.rs](https://img.shields.io/docsrs/lesto)](https://docs.rs/lesto)
[![CI](https://github.com/alesanfra/lesto/actions/workflows/ci.yml/badge.svg)](https://github.com/alesanfra/lesto/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/lesto.svg)](https://github.com/alesanfra/lesto/blob/main/LICENSE)

**FastAPI's ergonomics for Rust, on [axum](https://github.com/tokio-rs/axum).** Write an `async fn`;
get validation, RFC 9457 errors and an OpenAPI 3.1 document derived from its types, with no
annotations.

[Tutorial](https://alesanfra.github.io/lesto/) · [API docs](https://docs.rs/lesto) ·
[Changelog](https://github.com/alesanfra/lesto/blob/main/CHANGELOG.md)

```rust
use lesto::prelude::*;

#[lesto::model]
struct CreateUser {
    #[garde(length(min = 1, max = 64))]
    name: String,
    #[garde(email)]
    email: String,
}

#[lesto::model]
struct User { id: u64, name: String, email: String }

/// Create a user.
#[lesto::post("/users", status = 201)]
async fn create_user(Json(user): Json<CreateUser>) -> Json<User> {
    Json(User { id: 1, name: user.name, email: user.email })
}

/// Fetch a user.
#[lesto::get("/users/{id}", responses(404))]
async fn get_user(Path(id): Path<u64>) -> Result<Json<User>, HttpError> {
    Err(HttpError::not_found(format!("user {id} not found")))
}

#[lesto::main]
async fn main() -> std::io::Result<()> {
    App::new()
        .title("Users API")
        .routes(routes![create_user, get_user])
        .serve()
        .await
}
```

Interactive docs at <http://127.0.0.1:8000/docs>, and bad input answered for you:

```console
$ curl -X POST localhost:8000/users -H 'content-type: application/json' -d '{"name": "", "email": "nope"}'
HTTP/1.1 422 Unprocessable Entity
content-type: application/problem+json

{"type":"about:blank","title":"Unprocessable Entity","status":422,"detail":"2 validation errors",
 "instance":"/users","errors":[
   {"in":"body","pointer":"/email","detail":"not a valid email: value is missing `@`","code":"value_error"},
   {"in":"body","pointer":"/name","detail":"length is lower than 1","code":"value_error"}]}
```

## Get started

```sh
cargo add lesto            # serde, schemars, garde and tokio come with it
cargo install lesto-cli
lesto dev                  # rebuild and restart on save, the port stays open
```

## Batteries included

- **OpenAPI 3.1 from types**: arguments and return types become the document; Scalar at `/docs`,
  Swagger UI at `/swagger`.
- **Validation** with [garde](https://github.com/jprochazk/garde), one RFC 9457 error entry per
  failed check, with a JSON Pointer.
- **One model, many views**: `#[lesto::model(views(Create(..), Update(..)))]` generates the
  create and update types.
- **Authentication** extractors (`Bearer`, `Basic`, `ApiKey`) that document themselves;
  OpenID Connect JWT verification with `oidc`.
- **Databases** with `db`: sqlx stores, one transaction per method, permissions checked before
  the query, Postgres row level security.
- **MCP** with `mcp`: routes served to AI agents as tools, resources and prompts.
- **Logs** on stdout out of the box, one line per request, text or JSON (`LESTO_LOG=json`).
- **OpenTelemetry** with `otel`: traces, logs and metrics, configured by the `OTEL_*` variables.
- **AWS Lambda** with `lambda`: the same app behind API Gateway, Function URLs or ALB.
- **Production defaults**: graceful shutdown, timeouts, body limits, CORS, compression, request
  ids, panics answered as `500` problems.
- **Plain axum underneath**: a lesto app is an `axum::Router`, and an axum router mounts in a
  lesto app. [Leaving](https://alesanfra.github.io/lesto/D-leaving-lesto.html) is a documented
  path.

All of it costs about 0.3 µs per request over the axum router it builds
([measurements](https://alesanfra.github.io/lesto/E-performance.html)).

## How it compares

| | lesto | axum + utoipa / aide | poem-openapi | dropshot | loco.rs |
|---|---|---|---|---|---|
| Built on | axum | axum | poem | hyper | axum |
| OpenAPI | from handler types, no annotations | utoipa: annotations; aide: from types | from types | from types, spec-first | via utoipa |
| Request validation | built in (garde) | third party | built in | via types | third party |
| Error format | RFC 9457 | your own | typed enums | own shape | framework-defined |
| Data layer | sqlx stores, permissions, RLS | — | — | — | SeaORM, full stack |
| Observability | OTel traces, logs, metrics | — | — | — | — |

lesto is for JSON APIs: API-first like poem-openapi and dropshot, with batteries like loco.rs,
on axum.

## Project

Semver; lesto 0.1 tracks axum 0.8, and the crates it re-exports are part of its API. MSRV: Rust
1.94. Contributing, design decisions and the roadmap:
[AGENTS.md](https://github.com/alesanfra/lesto/blob/main/AGENTS.md).

Copyright 2026 Alessio Sanfratello. Licensed under the
[Apache License, Version 2.0](https://github.com/alesanfra/lesto/blob/main/LICENSE); contributions
are accepted under the same license.

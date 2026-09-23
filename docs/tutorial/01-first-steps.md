# 1. First steps

## Create the project

```sh
cargo new hello-lesto
cd hello-lesto
```

Open `Cargo.toml` and add the dependencies. Besides `lesto` you need `serde`, `garde` and
`schemars`: their derive macros (`#[derive(Serialize)]`, `#[derive(Validate)]`, ...) must find
them as direct dependencies of your crate, exactly as happens with `serde` in any Rust project.
The async runtime is not on the list: `#[lesto::main]` and `#[lesto::test]` are tokio's
`#[tokio::main]` and `#[tokio::test]` through lesto's own copy of tokio (`lesto::tokio`), with
the same options (`#[lesto::main(flavor = "current_thread")]`).

```toml
[dependencies]
lesto = { path = "../lesto/crates/lesto" }   # or the published version
serde = { version = "1", features = ["derive"] }
garde = { version = "0.23", features = ["derive"] }
schemars = { version = "1", features = ["derive"] }
```

## The first route

Replace `src/main.rs`:

```rust
use lesto::prelude::*;

/// Greets the world.
#[lesto::get("/hello")]
async fn root() -> &'static str {
    "Hello, lesto!"
}

#[lesto::main]
async fn main() -> std::io::Result<()> {
    App::new()
        .title("Hello API")
        .routes(routes![root])
        .serve()
        .await
}
```

Run it with the lesto CLI (once: `cargo install lesto-cli`, or from a checkout of this repository
`cargo install --path crates/lesto-cli` until it is published):

```sh
lesto dev
```

`lesto dev` builds the project, starts it, and **rebuilds and restarts it every time you save a
file**, like `fastapi dev`. It also keeps the port open while the code compiles, so a request sent
during a rebuild waits for the new version instead of failing. Plain `cargo run` works too, without
the reload.

`lesto openapi` prints the OpenAPI document instead of serving it (`lesto openapi -o api.json`
writes a file), for client generators and CI checks. It runs your `main` up to `App::serve`, which
writes the document and returns.

In another terminal:

```sh
curl http://127.0.0.1:8000/hello
```

```
Hello, lesto!
```

## What you wrote

Line by line:

- `use lesto::prelude::*` imports everything a route file normally needs: `App`, `routes!`, the
  extractors (`Json`, `Query`, `Path`, `State`), `HttpError` and the serde, garde and schemars
  derives.
- `#[lesto::get("/hello")]` declares a `GET /hello` route. There are also `post`, `put`, `patch`,
  `delete`, `head`, `options`.
- `/// Greets the world.` is not just a comment: it becomes the operation's *summary* in the
  documentation. A following paragraph, separated by a blank line, becomes the *description*
  (Markdown allowed).
- `async fn root() -> &'static str`: the return type decides both the HTTP response and its
  documentation. A string is `text/plain`.
- `App::new()...routes(routes![root])` registers the routes. `routes![]` takes a list of
  annotated functions.
- `.serve()` binds `LESTO_HOST:LESTO_PORT` (default `127.0.0.1:8000`; `PORT` alone also works,
  for Heroku and Cloud Run) and serves until `Ctrl-C` or `SIGTERM`. The shutdown is graceful:
  the port closes at once, requests already running finish, then `serve` returns.
  `.serve_at("0.0.0.0:9000")` takes an explicit address. Under `lesto dev` (or systemd
  socket activation) it serves on the socket it inherits instead; the address is then ignored.

## Automatic documentation

Open <http://127.0.0.1:8000/docs>: that is the **Scalar** API reference, generated from the code
you just wrote. You will see `GET /hello` with the summary "Greets the world." and a `200` 
response of type `text/plain`.

Open <http://127.0.0.1:8000/swagger>: the same documentation with the classic **Swagger UI**.

Open <http://127.0.0.1:8000/openapi.json>: the raw **OpenAPI 3.1** document that feeds both pages.
You can hand it to any client generator. The `operationId` is `{function}_{path}_{method}`
with every non-alphanumeric character turned into `_` (FastAPI's convention), so it stays unique
when two modules have a `list` function; `operation_id = "..."` in the attribute overrides it.

```json
{
  "openapi": "3.1.0",
  "info": { "title": "Hello API", "version": "0.1.0" },
  "paths": {
    "/hello": {
      "get": {
        "summary": "Greets the world.",
        "operationId": "hello_hello_get",
        "responses": {
          "200": {
            "description": "OK",
            "content": { "text/plain; charset=utf-8": { "schema": { "type": "string" } } }
          }
        }
      }
    }
  }
}
```

You did not write a line of OpenAPI: it is derived from the function's **types**. That is the
thread running through the whole tutorial.

## API metadata

```rust
App::new()
    .title("Hello API")
    .version("1.0.0")
    .description("An example API. Supports **Markdown**.")
    .server("https://api.example.com")
```

To move or disable the pages:

```rust
App::new()
    .docs_url(Some("/reference")) // Scalar elsewhere
    .swagger_url(None)            // no Swagger UI
    .openapi_url(None)            // no document: disables the pages too
```

## Recap

- A route is an `async fn` with `#[lesto::get("/path")]`.
- The doc comment becomes the operation's description.
- The return type decides response and documentation.
- `/docs`, `/swagger` and `/openapi.json` come for free.

Next: [Path parameters](02-path-parameters.md).

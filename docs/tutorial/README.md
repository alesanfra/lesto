# lesto tutorial

This tutorial takes you from zero to a complete API, one concept at a time. Every chapter stands
on its own: it has the full code, the command to try it, and what you will see in the interactive
documentation. If you know FastAPI you will feel at home; if you don't, you don't need to.

Prerequisites: stable Rust (`rustup`), `curl` and a browser.

| # | Chapter | What you learn |
|---|---|---|
| 1 | [First steps](01-first-steps.md) | Create the project, the first route, automatic documentation |
| 2 | [Path parameters](02-path-parameters.md) | `Path<T>`, types, several parameters, what happens when the type doesn't fit |
| 3 | [Query parameters](03-query-parameters.md) | `Query<T>`, optionals, defaults, validation |
| 4 | [Request body](04-request-body.md) | `Json<T>`, deserialization and validation with garde |
| 5 | [Validation](05-validation.md) | garde rules, nested structures, custom rules |
| 6 | [Responses](06-responses.md) | Return types, status codes, `Result`, empty responses |
| 7 | [Error handling](07-errors.md) | `HttpError`, RFC 9457, documented errors, FastAPI format |
| 8 | [State and dependencies](08-state-and-dependencies.md) | `State<S>`, `Extension`, custom extractors (lesto's `Depends`) |
| 9 | [Security](09-security.md) | Bearer, API keys, Basic, OAuth2, verifying tokens |
| 10 | [Bigger projects](10-bigger-projects.md) | Modules, `nest`, tags, route order |
| 11 | [Middleware and axum](11-middleware-and-axum.md) | tower layers, CORS, logging, dropping down to axum |
| 12 | [Testing](12-testing.md) | Testing the API without opening a port |
| 13 | [Databases](13-databases.md) | sqlx stores with the `db` feature: transactions, principals, permissions |
| 14 | [AWS Lambda](14-aws-lambda.md) | The same binary behind API Gateway with the `lambda` feature |
| A | [From FastAPI to lesto](A-from-fastapi-to-lesto.md) | Correspondence table |
| B | [Common problems](B-common-problems.md) | Compile errors and how to fix them |
| C | [Why axum and not actix-web](C-why-axum.md) | The reasons behind the choice of core |

Every snippet in this tutorial is compiled and tested in `examples/99-tutorial` (`cargo test -p tutorial`),
so the documentation cannot drift from the API. The other examples grow with the chapters:
`examples/01-hello` (chapter 1), `examples/02-notes` (a complete CRUD API, chapter 13),
`examples/03-lambda` (chapter 14).

To read this tutorial as a website (sidebar, search, dark theme):
`cargo install mdbook && mdbook serve docs --open`.

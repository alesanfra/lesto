# 07-cloudflare-d1

lesto on Cloudflare Workers with a D1 database: the notes API (`GET /notes`, `GET /notes/{id}`,
`POST /notes`), the OpenAPI document at `/openapi.json`, Scalar at `/docs` and Swagger UI at
`/swagger`, all compiled to WebAssembly.

This package is **not** a member of the workspace (`exclude` in the root `Cargo.toml`): it builds
for `wasm32-unknown-unknown` only, so `cargo test --workspace` cannot compile it on the host.
Work from this directory; it has its own `Cargo.lock`.

## Prerequisites

- Rust with the wasm target: `rustup target add wasm32-unknown-unknown`
- `cargo install worker-build`
- Node.js, for `npx wrangler` (no global install needed)

## Run locally

```sh
cd examples/07-cloudflare-d1
worker-build --release
npx wrangler d1 migrations apply lesto-notes --local
npx wrangler dev --local
# in another terminal
curl -s localhost:8787/notes
curl -s -X POST localhost:8787/notes -H 'Content-Type: application/json' -d '{"text":"hello"}'
curl -s localhost:8787/notes/1
```

An empty `text` answers a `422` problem, an unknown id a `404` problem.

## Deploy

```sh
npx wrangler login
npx wrangler d1 create lesto-notes          # paste the printed database_id in wrangler.toml
npx wrangler d1 migrations apply lesto-notes --remote
npx wrangler deploy
```

## How it works

The `fetch` handler in `src/lib.rs` hands every request to the axum router that
`App::into_router` builds, once per isolate. The Worker's `Env` is put in the request extensions
(wrapped in `SendWrapper`) and the handlers read the `DB` binding from there. There is no
`App::serve`: Workers have no sockets.

## Caveats

- Not available on wasm32: `App::serve` and friends, `bind_address`, `listener`,
  `shutdown_signal`, the `ConnectInfo` peer address in request spans. Only `App::into_router`.
- `App::timeout` and any other feature built on tokio's timers do not work: tokio's `time`
  driver needs a runtime, and Workers provide none. Use Cloudflare's own limits.
- Build lesto with `default-features = false`: the `log` feature prints on stdout with
  `tracing-subscriber`; Workers logs go through `console`. Install your own `tracing`
  subscriber if you want them.
- `db`, `lambda`, `oidc`, `otel` and `mcp` are not supported on this target. Use the D1 binding
  directly, as this example does.
- A panic aborts the WebAssembly instance, so the catch-panic layer cannot turn it into a `500`
  problem the way it does natively; `console_error_panic_hook` at least logs it.
- Compression (`App::compression`) is not recommended: Cloudflare compresses at the edge.

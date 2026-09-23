# OAuth2 / OpenID Connect: validating JWTs by configuration

*Written 2026-09-23 on branch `oidc` (from `main` at `d71d8fd`, after PR #1). Roadmap item 2 in
`AGENTS.md` and `README.md`. The design below was agreed with the maintainer; the open points
have a recommended answer, which is the one to implement unless the maintainer says otherwise.*

## How to use this file in a fresh session

1. Read `AGENTS.md` first: conventions, design decisions, commands.
2. Work on branch `oidc`. Do the tasks in order, one commit per task (imperative subject, body
   explains why, the attribution line from the session's instructions).
3. Gate before every commit (same as `docs/review/plan-production-ready.md`):

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo check -p lesto --no-default-features            # and --features db / otel / oidc, each alone
sh docs/build.sh                                      # when docs changed
cargo +1.94 check --workspace --all-targets --all-features --profile ci   # MSRV
cargo deny check                                      # new dependencies: licenses
git grep -i presto                                    # only the historical mention in AGENTS.md
```

4. Tick the box here with the decision or the numbers, commit. At the end: push, open the PR
   against `main` (`gh pr create`), body ending with the Claude Code line.

Docker is available (Postgres: do not touch `pethelp-postgres` on 5432, start your own on another
port; OpenObserve/Jaeger: `examples/04-opentelemetry/compose.yaml`). An OIDC provider for the end
to end check can be run the same way (Keycloak, or `ghcr.io/navikt/mock-oauth2-server`).

## Target API

```rust
// main: discovery once at startup
let auth = lesto::oidc::Oidc::discover("https://login.example.com/.well-known/openid-configuration")
    .audiences(["api://notes"])     // optional: `aud` must contain one of these
    .clients(["web", "cli"])        // optional: the client claim must be one of these
    .await?;                        // fetches the discovery document and the JWKS

App::new()
    .oidc(auth)                     // shared verifier + the `openIdConnect` security scheme
    .routes(routes![me])

// or from the environment, like OTel: LESTO_OIDC_DISCOVERY_URL, LESTO_OIDC_AUDIENCES,
// LESTO_OIDC_CLIENTS (comma separated)
let auth = lesto::oidc::Oidc::from_env().await?;   // Option<Oidc> or an error, see task 6

#[lesto::get("/me")]
async fn me(token: Jwt<Claims>) -> Json<Claims> {  // Claims: the user's Deserialize type
    Json(token.into_claims())
}
```

## Decisions (agreed; do not reopen without the maintainer)

1. **Feature `oidc`, off by default.** Discovery and JWKS need an async HTTP client with TLS
   (reqwest + rustls). That is heavier than `otel`, which deliberately has no TLS, so it must not
   be in every build. Check whether the `reqwest` already in `Cargo.lock` (0.13, pulled by
   `otel` with the blocking client) can be shared with the async client + `rustls-tls` features;
   one reqwest in the graph is the goal. Prefer `jsonwebtoken` for decoding/verifying (check the
   current major on crates.io and its crypto backend feature: pick the rustls-compatible one,
   no OpenSSL). Every new crate must pass `cargo deny check`.
2. **Extractor `Jwt<C = StandardClaims>`**: `C: DeserializeOwned`. `StandardClaims` holds
   `iss`, `sub`, `aud`, `exp`, `nbf`, `iat`, `scope`/`scp`, plus a `serde_json::Map` for the
   rest (key order preserved, as everywhere). Accessors: `claims()`, `into_claims()`,
   `subject()`, `scopes()`. It implements `FromRequestParts<S>` (verifier found through
   `FromRef<S>` for the state, or an `Extension` installed by `App::oidc`, choose one and
   document why) and `OperationInput` (documents the `openIdConnect` scheme on the operation).
3. **What is verified, in order, before the handler runs** (any failure: `401` problem with
   `WWW-Authenticate: Bearer error="invalid_token", error_description=".."`; a missing header:
   `401` with plain `Bearer` challenge, as `Bearer` does today):
   - signature, with the JWKS key named by the header's `kid`;
   - `alg` in the provider's `id_token_signing_alg_values_supported` ∩ asymmetric algorithms;
     `none` and `HS*` always refused (key confusion);
   - `iss` equal to the discovery document's `issuer`;
   - `aud` contains one of the configured audiences, when configured;
   - client claim among the configured clients, when configured: read **`azp`, then
     `client_id` (RFC 9068), then `appid` (Entra ID v1)**, first present wins;
   - `exp` and `nbf` with a leeway (default 30 s, `.leeway(Duration)`).
   The detail of a failure goes to `tracing` at `debug`; the response says only which class
   (expired, bad signature, wrong audience...), never echoes the token.
4. **JWKS cache**: in memory, shared (`Arc`), read lock on the hot path, no HTTP per request.
   Refetched when a token carries an unknown `kid` (key rotation), **rate limited** (at most one
   refetch per `min_refresh`, default 60 s) so forged `kid`s cannot make the service hammer the
   provider. Also honor `Cache-Control: max-age` of the JWKS response if cheap. A refetch that
   fails keeps the old keys and logs at `warn`.
5. **OpenAPI**: `SecurityScheme::open_id_connect(discovery_url)` (exists in `openapi.rs`), so
   Scalar / Swagger show the login. Scopes: `Jwt` documents none by default; a scope-requiring
   variant is task 8.
6. **Environment configuration**, following the "configured by the environment" decision in
   `AGENTS.md`: `Oidc::from_env()` reads `LESTO_OIDC_DISCOVERY_URL` (unset: `Ok(None)`),
   `LESTO_OIDC_AUDIENCES`, `LESTO_OIDC_CLIENTS`, `LESTO_OIDC_LEEWAY_SECS`. Parse through a getter
   function like `otel::Config::read` so tests never write the process environment.
7. **Discovery at startup, failing loudly**: `discover(..).await` returns an error if the
   document or the JWKS cannot be fetched or parsed (the app should not start half-configured).
   No panics. Only `https` discovery URLs, except `http://localhost` / `127.0.0.1` for tests.
8. **Group protection**: `App::protect(auth)` on an `App` (typically one that is then
   `nest`ed): every route in it requires a valid token and documents the scheme, without a
   `Jwt` argument in each handler. This is the "first piece of an easy middleware system" the
   roadmap asks for: implement it as a layer applied to that app's router plus an app-wide
   security requirement (like `App::security`). Keep it small; no generic middleware framework.
9. **`lesto::db` integration**: `impl Authenticated for User { type Credential = Jwt<Claims>; .. }`
   must just work (`Jwt` is `FromRequestParts + OperationInput`). `has_permission` typically
   checks `scope`/`scp`/`roles`: show it in the tutorial, do not hard-code a mapping.

## Tasks

- [ ] **1. Dependencies and feature.** `oidc` feature in `crates/lesto/Cargo.toml`, workspace
      dependencies for the JWT crate and reqwest (async + rustls). `cargo tree` shows one
      reqwest major; `cargo deny check` passes; `cargo check -p lesto --no-default-features
      --features oidc` compiles. Record the chosen crates and why here.
- [ ] **2. Discovery and JWKS.** `src/oidc/` (module layout like `src/db/`): fetch and parse the
      discovery document (`issuer`, `jwks_uri`, `id_token_signing_alg_values_supported`), fetch
      the JWKS, build the key set. `Oidc::discover`, `.audiences`, `.clients`, `.leeway`.
      Errors are an enum (`lesto::oidc::Error`), not `anyhow`. Unit tests with a local HTTP server
      (axum on `127.0.0.1:0` serving fixed JSON) — no network in tests.
- [ ] **3. Verification.** A `Verifier` (inside `Oidc`) with every check of decision 3. Tests
      sign tokens with a test RSA/EC key (generated in the test or a checked-in PEM under
      `tests/fixtures/`): valid, expired, not yet valid, wrong issuer, wrong audience, wrong
      client (each of `azp`/`client_id`/`appid`), unknown `kid`, `alg: none`, `HS256` with the
      public key as secret (must fail), tampered payload.
- [ ] **4. JWKS refresh.** Unknown `kid` → refetch, rate limited; failed refetch keeps old keys.
      Test: rotate the key on the local server, a token with the new `kid` is accepted after one
      refetch; a burst of unknown `kid`s triggers one fetch per `min_refresh`.
- [ ] **5. `Jwt<C>` extractor + `App::oidc`.** 401 problems as in decision 3 (RFC 9457, through
      `HttpError`, `WWW-Authenticate` header). `OperationInput` documents the `openIdConnect`
      scheme. Integration tests in `tests/oidc.rs` through `oneshot`. The UI tests: a
      `Jwt<C>` with `C` not `Deserialize` gets an `on_unimplemented` message that says what to
      do (add a `tests/ui/oidc_*.rs` case + `.expected`).
- [ ] **6. Environment.** `Oidc::from_env()` + `Config::read(getter)`, tests with a map getter.
- [ ] **7. `lesto::db`.** A test in `tests/db.rs` (or `tests/oidc.rs`) with
      `type Credential = Jwt<Claims>` and a permission from `scope`.
- [ ] **8. `App::protect(auth)`** (+ optional required scopes, e.g. `.protect(auth.scopes(["notes:read"]))`,
      403 problem when a scope is missing). Test: a nested protected app, a public route next
      to it, the OpenAPI document shows `security` only on the protected operations.
- [ ] **9. Docs.** Tutorial chapter 9 (Security): new section "OpenID Connect" (discovery,
      audiences, clients, `Jwt<C>`, `protect`, env vars, what is verified, key rotation);
      chapter 13: the `Authenticated` example with `Jwt`; README feature list; `AGENTS.md`:
      layout (`src/oidc/`, `tests/oidc.rs`), a "Design decisions" entry (the choices above and
      why), the compile-errors table if a new diagnostic was added, remove roadmap item 2 from
      both `AGENTS.md` and `README.md` (keep the two lists in sync). Every tutorial snippet
      compiled in `examples/99-tutorial` (enable `oidc` there) — use a local discovery server in
      its tests, never a real provider. `CHANGELOG.md`: Added.
- [ ] **10. End to end.** Run a real provider in Docker (mock-oauth2-server or Keycloak), get a
      token with client credentials, call a protected route of an example (new
      `examples/06-oidc` or an `oidc` variant of an existing one), check 200 / 401 / 403.
      Document the commands in the example's README. Not in CI (needs Docker), like
      `04-opentelemetry`.
- [ ] **11. PR.** Push `oidc`, open the PR against `main`; body: what, decisions, breaking
      changes (none expected), verification (gate, e2e), what is left.

## Out of scope for this branch

Token introspection (RFC 7662) for opaque tokens, issuing tokens, the authorization-code flow /
login pages, refresh tokens, mTLS-bound tokens, DPoP, multiple issuers at once (note it as a
possible follow-up if the design makes it easy: `Oidc` values in a list).

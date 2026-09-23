# 06 — OpenID Connect: verified bearer tokens

Chapter 9 of the tutorial against a real provider. The API has a public route (`/health`), a
route that takes a `Jwt` (`/me`), and a nested app protected as a whole that needs the `admin`
scope (`/admin/stats`). The verifier is configured by the environment (`Oidc::from_env`).

The provider is [mock-oauth2-server](https://github.com/navikt/mock-oauth2-server) in Docker:
a real OpenID Connect server (discovery document, JWKS, signed tokens) that accepts any client.
`compose.yaml` makes its client-credentials tokens carry `aud: api://notes`, `azp: notes-cli`,
and the `admin` scope only when the client asks for it.

```sh
cd examples/06-oidc
docker compose up -d

export LESTO_OIDC_DISCOVERY_URL=http://localhost:8089/default/.well-known/openid-configuration
export LESTO_OIDC_AUDIENCES=api://notes
export LESTO_OIDC_CLIENTS=notes-cli
export LESTO_PORT=8765
cargo run -p oidc-example
```

Plain `http` is accepted because the provider is on `localhost`; anywhere else lesto requires
`https`. In another terminal:

```sh
sh verify.sh
```

```
ok   200 public route, no token
ok   401 Jwt argument, no token
ok   401 Jwt argument, forged token
ok   200 Jwt argument, reader token
ok   401 protected app, no token
ok   403 protected app, reader token
ok   200 protected app, admin token
ada (notes:read admin)
```

By hand:

```sh
TOKEN=$(curl -s -X POST http://localhost:8089/default/token \
  -d grant_type=client_credentials -d client_id=notes-cli -d client_secret=secret -d scope=admin \
  | tr -d '\n' | sed -E 's/.*"access_token" *: *"([^"]+)".*/\1/')
curl -i http://127.0.0.1:8765/admin/stats -H "Authorization: Bearer $TOKEN"
```

Scalar at <http://127.0.0.1:8765/docs> shows the `openIdConnect` scheme on `/me` and
`/admin/stats`, not on `/health`.

This example is not run in CI, but its checks are: `crates/lesto/tests/oidc_provider.rs` makes
the same calls against the same provider, which CI starts as a service (with the `JSON_CONFIG`
of `compose.yaml`: keep the two in sync). Locally, with the container up:

```sh
LESTO_TEST_OIDC_URL=http://localhost:8089/default cargo test -p lesto --test oidc_provider
```

`crates/lesto/tests/oidc.rs` covers every check with no network at all.

`docker compose down` when done.

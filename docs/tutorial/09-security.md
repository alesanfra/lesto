# 9. Security

lesto ships extractors for the three most common ways of authenticating an HTTP request. Each
does two things: it extracts the credentials and **registers the security scheme** in the
documentation, so Scalar and Swagger UI show the *Authorize* button and a lock on protected
operations.

The extractors **do not verify** credentials: they read the token and hand it to you. Verification
(session lookup, key comparison) is yours, usually inside a custom extractor like `CurrentUser`
from the previous chapter. The exception is a JWT issued by an OpenID Connect provider: with the
`oidc` feature lesto verifies it for you ([below](#openid-connect-verified-tokens)).

## Bearer token

```rust
use lesto::prelude::*;

/// Who is calling?
#[lesto::get("/me")]
async fn me(auth: Bearer) -> String {
    format!("token: {}", auth.token())
}
```

```sh
curl http://127.0.0.1:8000/me -H 'Authorization: Bearer abc123'
```

```
token: abc123
```

Without the header:

```sh
curl -i http://127.0.0.1:8000/me
```

```
HTTP/1.1 401 Unauthorized
www-authenticate: Bearer
content-type: application/problem+json

{"type":"about:blank","title":"Unauthorized","status":401,"detail":"Not authenticated","instance":"/me"}
```

The documentation gains `components.securitySchemes.bearerAuth` of type `http`/`bearer`, and the
operation gets `security: [{"bearerAuth": []}]` plus a `401` response.

## API key

A key in a header, a query parameter or a cookie. The key's name varies from API to API, so you
declare it with a **marker type**:

```rust
struct AdminKey;

impl ApiKeyScheme for AdminKey {
    const NAME: &'static str = "adminKey";      // scheme name in the documentation
    const KEY: &'static str = "X-Admin-Key";    // header name
    // const LOCATION: ApiKeyIn = ApiKeyIn::Header;   default; also Query or Cookie
}

/// Delete everything. Requires the admin key.
#[lesto::delete("/everything", status = 204, responses(403))]
async fn nuke(key: ApiKey<AdminKey>) -> Result<(), HttpError> {
    if key.key() != std::env::var("ADMIN_KEY").unwrap_or_default() {
        return Err(HttpError::forbidden("invalid admin key"));
    }
    Ok(())
}
```

```sh
curl -X DELETE http://127.0.0.1:8000/everything -H 'X-Admin-Key: ...'
```

For a session cookie:

```rust
struct Session;
impl ApiKeyScheme for Session {
    const NAME: &'static str = "session";
    const KEY: &'static str = "sid";
    const LOCATION: ApiKeyIn = ApiKeyIn::Cookie;
}
```

## Basic

```rust
#[lesto::get("/admin")]
async fn admin(creds: Basic) -> Result<String, HttpError> {
    if creds.username != "admin" || creds.password != "s3cret" {
        return Err(HttpError::unauthorized("bad credentials"));
    }
    Ok("welcome".into())
}
```

```sh
curl -u admin:s3cret http://127.0.0.1:8000/admin
```

Note: comparing passwords with `!=` is fine in a tutorial, not in production. Use a constant-time
comparison (`subtle`, `constant_time_eq`) and hashed passwords.

## OpenID Connect: verified tokens

When the tokens come from an identity provider (Keycloak, Auth0, Okta, Entra ID, Cognito,
Google...), lesto can verify them before the handler runs. Turn on the `oidc` feature:

```toml
lesto = { version = "0.1", features = ["oidc"] }
```

and point lesto at the provider's discovery document. `discover` fetches it and the provider's
keys (the JWKS) once, at startup:

```rust
use lesto::oidc::{Jwt, Oidc, StandardClaims};

/// Who is calling, according to the token.
#[lesto::get("/me")]
async fn me(token: Jwt) -> Json<StandardClaims> {
    Json(token.into_claims())
}

#[lesto::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let auth = Oidc::discover("https://login.example.com/.well-known/openid-configuration")
        .audiences(["api://notes"])     // optional: `aud` must contain one of these
        .clients(["web", "cli"])        // optional: the token must be issued to one of these
        .await?;
    App::new().oidc(auth).routes(routes![me]).serve().await?;
    Ok(())
}
```

A `Jwt` argument is a token that passed every check, in this order (the first failure is the one
reported):

1. the **algorithm** in the token's header: one the provider announces, and asymmetric. `none`
   and `HS256`/`HS384`/`HS512` are always refused (an `HS*` token "signed" with the public key is
   the classic forgery);
2. the **key**: the one of the JWKS that the token's `kid` names, for that algorithm;
3. the **signature**, with that key;
4. **`exp`** and **`nbf`**, with 30 seconds of leeway for clock skew (`.leeway(Duration)`);
5. **`iss`**: the provider's issuer, as the discovery document states it;
6. **`aud`** contains one of the `audiences`, when you configured some;
7. the **client**, when you configured `clients`: `azp`, then `client_id` (RFC 9068), then `appid`
   (Entra ID v1), the first present wins.

A missing token answers `401` with `WWW-Authenticate: Bearer`, like `Bearer`. A token that fails a
check answers `401` with `Bearer error="invalid_token"` and a detail naming the class of the
failure:

```
HTTP/1.1 401 Unauthorized
www-authenticate: Bearer error="invalid_token", error_description="The token has expired"
content-type: application/problem+json

{"type":"about:blank","title":"Unauthorized","status":401,"detail":"The token has expired","instance":"/me"}
```

The detail (the verification library's error, the unknown `kid`, the client the token named) goes
to the log at `debug`, never to the client, and the token is never logged.

### Your own claims

`Jwt` alone reads `StandardClaims` (`iss`, `sub`, `aud`, `exp`, `scope`/`scp` and the rest in
`extra`). For the claims your provider adds, name a type that deserializes (`#[lesto::model]`,
chapter 4, or your own `#[derive(Deserialize)]`):

```rust
#[lesto::model]
struct Claims {
    sub: String,
    email: String,
}

#[lesto::get("/email")]
async fn my_email(token: Jwt<Claims>) -> String {
    token.claims().email.clone()
}
```

A valid token whose claims do not fit the type answers `401`. Whatever the type, `token.subject()`,
`token.scopes()`, `token.has_scope("notes:write")` and `token.claim("tenant")` read the raw claims.

### Protecting a group of routes

A `Jwt` argument protects one handler. `App::protect` protects every route of an app, whatever
the handlers take; nest that app and the routes next to it stay public:

```rust
let admin = App::new()
    .routes(routes![stats, purge])
    .protect(auth.clone().scopes(["admin"]));   // or `.protect(auth.clone())`: any valid token

App::new()
    .oidc(auth)
    .routes(routes![health, me])
    .nest("/admin", admin)
```

No token or a bad one answers `401`; a valid token without every scope listed answers `403` with
`WWW-Authenticate: Bearer error="insufficient_scope", scope="admin"`. Scopes are read from `scope`
(space separated) and `scp` (Entra ID, Okta). A `Jwt` argument inside a protected app reads the
claims the layer already verified. The documentation routes are never protected; every other route
of the app is, including one marked `public` (see [below](#default-security-for-the-whole-api)).

### The documentation

`App::oidc` and `App::protect` register the `openIdConnect` security scheme pointing at the
discovery URL, so Scalar and Swagger UI offer a login; each operation that takes a `Jwt` or sits
in a protected app carries `security: [{"openIdConnect": [..]}]` and its `401` (and `403`).

### From the environment

```rust
let auth = Oidc::from_env().await?;          // Option<Oidc>
let app = match auth {
    Some(auth) => app.oidc(auth),
    None => app,
};
```

| Variable | Meaning |
|---|---|
| `LESTO_OIDC_DISCOVERY_URL` | the discovery document; unset, `from_env` returns `Ok(None)` |
| `LESTO_OIDC_AUDIENCES` | accepted audiences, comma separated |
| `LESTO_OIDC_CLIENTS` | accepted clients, comma separated |
| `LESTO_OIDC_LEEWAY_SECS` | leeway on `exp` and `nbf`, in seconds (default 30) |

### Startup and key rotation

`discover` fails, and the application does not start, if the discovery document or the keys cannot
be fetched or parsed, if the document names an issuer other than the URL it was found under (for a
URL ending in `/.well-known/openid-configuration`, as the specification requires), or if the
provider announces only algorithms lesto refuses. Only `https` URLs are accepted, plus plain
`http` on `localhost` and loopback addresses (a provider next to the application, tests).

The keys stay in memory; requests never wait for the provider. When a token names a `kid` lesto
does not know, the provider has probably rotated its keys: lesto fetches the JWKS again, **at most
once a minute** (`.min_refresh(Duration)`), so a flood of tokens with made-up `kid`s cannot turn
your service into a client hammering the provider. A `Cache-Control: max-age` on the JWKS
response is honored too. A fetch that fails keeps the keys already known and logs a warning.

Out of scope: opaque tokens (introspection), issuing tokens, login pages, several issuers at once.

## Other OAuth2 schemes

For schemes that have URLs and scopes, implement `AuthScheme` on a marker and pass it to `Bearer`:

```rust
struct OAuth2;

impl AuthScheme for OAuth2 {
    const NAME: &'static str = "oauth2";

    fn scheme() -> SecurityScheme {
        SecurityScheme::oauth2(
            OAuthFlows::password("/token")
                .scope("items:read", "Read items")
                .scope("items:write", "Create and modify items"),
        )
    }

    /// Scopes required by the operations that use this marker.
    fn scopes() -> &'static [&'static str] {
        &["items:read"]
    }
}

#[lesto::get("/items")]
async fn list_items(auth: Bearer<OAuth2>) -> Json<Vec<Item>> { .. }
```

The operation becomes `security: [{"oauth2": ["items:read"]}]` and Swagger UI knows how to obtain
the token through the `password` flow. For different scopes create different markers
(`OAuth2Write`, ...).

Other constructors: `OAuthFlows::authorization_code(auth_url, token_url)`,
`OAuthFlows::client_credentials(token_url)`, `OAuthFlows::implicit(auth_url)`,
`SecurityScheme::open_id_connect(discovery_url)`, `SecurityScheme::bearer_jwt()`.

## The full pattern: token → user

Combine `Bearer` with a custom extractor (see chapter 8). The handler receives the user, not the
token:

```rust
#[lesto::get("/orders")]
async fn my_orders(CurrentUser(user): CurrentUser, State(db): State<Db>) -> Json<Vec<Order>> {
    Json(db.orders_of(user.id))
}
```

## Authentication done by a middleware

If a layer (for instance `tower_http::validate_request` or a reverse proxy) authenticates before
the handler is called, you can still document the requirement:

```rust
// 1. as a marker argument, extracting nothing
async fn handler(_: Security<BearerAuth>) { .. }

// 2. as an attribute option, referring to a scheme registered on the App
#[lesto::get("/reports", security(bearerAuth))]
async fn reports() { .. }

App::new()
    .security_scheme("bearerAuth", SecurityScheme::bearer_jwt())
```

Alternative requirements (any one is enough) and scopes: `security(oauth2 = ["items:write"], "adminKey")`.

## Default security for the whole API

```rust
App::new()
    .security_scheme("bearerAuth", SecurityScheme::bearer())
    .security("bearerAuth", &[])          // default for every operation
```

and for the exceptions:

```rust
#[lesto::get("/health", public)]         // emits security: [] on this operation
async fn health() -> &'static str { "ok" }
```

`public` only changes the documentation. It does not exempt a route from `App::protect`, which
checks every request of its app: put public routes in an app that is not protected.

## Recap

- `Bearer`, `ApiKey<S>`, `Basic`: they extract and document; verification is yours.
- `Jwt<C>` (feature `oidc`): a token verified against an OpenID Connect provider;
  `App::protect` requires one on a whole group of routes.
- One marker type per API key or OAuth2 scheme; `Bearer<MyScheme>` to use it.
- Missing credentials → `401` with `WWW-Authenticate`.
- `security(..)`, `public` and `App::security` for when authentication happens elsewhere.

Next: [Bigger projects](10-bigger-projects.md).

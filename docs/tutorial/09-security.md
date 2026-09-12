# 9. Security

lesto ships extractors for the three most common ways of authenticating an HTTP request. Each
does two things: it extracts the credentials and **registers the security scheme** in the
documentation, so Scalar and Swagger UI show the *Authorize* button and a lock on protected
operations.

The extractors **do not verify** credentials: they read the token and hand it to you. Verification
(JWT signature, session lookup, key comparison) is yours, usually inside a custom extractor like
`CurrentUser` from the previous chapter.

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

## OAuth2 and OpenID Connect

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

## Recap

- `Bearer`, `ApiKey<S>`, `Basic`: they extract and document; verification is yours.
- One marker type per API key or OAuth2 scheme; `Bearer<MyScheme>` to use it.
- Missing credentials → `401` with `WWW-Authenticate`.
- `security(..)`, `public` and `App::security` for when authentication happens elsewhere.

Next: [Bigger projects](10-bigger-projects.md).

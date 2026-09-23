//! Bearer JWTs verified against an OpenID Connect provider. Enabled by the `oidc` feature.
//!
//! Point lesto at the provider's discovery document: it fetches the document and the key set
//! (JWKS) once at startup, and from then on every [`Jwt`] argument is a token whose signature,
//! issuer, audience, client and lifetime have been checked before the handler runs.
//!
//! ```ignore
//! use lesto::oidc::{Jwt, Oidc, StandardClaims};
//!
//! #[lesto::get("/me")]
//! async fn me(token: Jwt) -> Json<StandardClaims> {
//!     Json(token.into_claims())
//! }
//!
//! #[lesto::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let auth = Oidc::discover("https://login.example.com/.well-known/openid-configuration")
//!         .audiences(["api://notes"])
//!         .clients(["web", "cli"])
//!         .await?;
//!     App::new().oidc(auth).routes(routes![me]).serve().await?;
//!     Ok(())
//! }
//! ```
//!
//! What is verified, in order: the algorithm, which must be one the provider announces and
//! asymmetric (`none` and `HS*` are always refused); the key, the one of the JWKS the header's
//! `kid` names; the signature; `exp` and `nbf`, with a [`leeway`](Discover::leeway); `iss`,
//! equal to the discovery document's `issuer`; `aud`, when [`audiences`](Discover::audiences)
//! are configured; the client, when [`clients`](Discover::clients) are configured (`azp`, then
//! `client_id`, then `appid`: the first present wins). A failure answers
//! `401` with `WWW-Authenticate: Bearer error="invalid_token"`; the reason goes to `tracing` at
//! `debug`, never into the response.
//!
//! Keys are cached in memory. A token naming a `kid` the cache does not hold makes lesto fetch
//! the JWKS again (the provider rotated its keys), at most once per
//! [`min_refresh`](Discover::min_refresh), so forged `kid`s cannot turn the service into a
//! client hammering the provider. A fetch that fails keeps the keys already known.
//!
//! [`App::oidc`](crate::App::oidc) makes the verifier available to every [`Jwt`] argument and
//! registers the `openIdConnect` security scheme; [`App::protect`](crate::App::protect)
//! requires a valid token on every route of an app without a `Jwt` argument in each handler.

mod claims;
mod config;
mod error;
mod extract;
mod keys;
mod protect;
mod verify;

use std::sync::Arc;
use std::time::Duration;

pub use claims::{Claims, StandardClaims};
pub use error::Error;
pub use extract::Jwt;
pub use protect::{Protect, ProtectLayer, ProtectService};
pub use verify::TokenError;

use crate::openapi::SecurityScheme;
use keys::KeyCache;

/// The name of the security scheme under `components.securitySchemes`.
pub const SCHEME_NAME: &str = "openIdConnect";

/// Default of [`Discover::leeway`].
pub const DEFAULT_LEEWAY: Duration = Duration::from_secs(30);

/// Default of [`Discover::min_refresh`].
pub const DEFAULT_MIN_REFRESH: Duration = Duration::from_secs(60);

/// How long a request to the provider may take, discovery and JWKS alike.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// A verifier for the tokens of one OpenID Connect provider.
///
/// Built with [`Oidc::discover`] (or [`Oidc::from_env`]), which fetches the discovery document
/// and the key set. Cheap to clone: clones share the key cache.
#[derive(Clone)]
pub struct Oidc {
    inner: Arc<Inner>,
}

struct Inner {
    discovery_url: String,
    issuer: String,
    audiences: Vec<String>,
    clients: Vec<String>,
    leeway: Duration,
    algorithms: Vec<jsonwebtoken::Algorithm>,
    keys: KeyCache,
}

impl std::fmt::Debug for Oidc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Oidc")
            .field("discovery_url", &self.inner.discovery_url)
            .field("issuer", &self.inner.issuer)
            .field("audiences", &self.inner.audiences)
            .field("clients", &self.inner.clients)
            .field("leeway", &self.inner.leeway)
            .field("algorithms", &self.inner.algorithms)
            .finish_non_exhaustive()
    }
}

impl Oidc {
    /// Start configuring a verifier for the provider whose discovery document is at `url`
    /// (`https://…/.well-known/openid-configuration`). Awaiting the result fetches the document
    /// and the key set, and fails if either cannot be fetched or parsed: an application should
    /// not start with authentication half configured.
    ///
    /// Only `https` URLs are accepted, plus `http` on `localhost` and loopback addresses, for
    /// tests and a provider running next to the application.
    pub fn discover(url: impl Into<String>) -> Discover {
        Discover {
            url: url.into(),
            audiences: Vec::new(),
            clients: Vec::new(),
            leeway: DEFAULT_LEEWAY,
            min_refresh: DEFAULT_MIN_REFRESH,
        }
    }

    /// [`discover`](Self::discover) configured by the environment: `LESTO_OIDC_DISCOVERY_URL`,
    /// `LESTO_OIDC_AUDIENCES` and `LESTO_OIDC_CLIENTS` (comma separated),
    /// `LESTO_OIDC_LEEWAY_SECS`. `Ok(None)` when `LESTO_OIDC_DISCOVERY_URL` is not set.
    pub async fn from_env() -> Result<Option<Oidc>, Error> {
        match config::read(|name| std::env::var(name).ok())? {
            Some(discover) => discover.await.map(Some),
            None => Ok(None),
        }
    }

    /// The discovery document's URL, as configured.
    pub fn discovery_url(&self) -> &str {
        &self.inner.discovery_url
    }

    /// The issuer every token must name (`iss`), from the discovery document.
    pub fn issuer(&self) -> &str {
        &self.inner.issuer
    }

    /// Require `scopes` on top of a valid token: the argument of
    /// [`App::protect`](crate::App::protect) for a group of routes that needs more than a login.
    pub fn scopes<I>(self, scopes: I) -> Protect
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        Protect::from(self).scopes(scopes)
    }

    /// The `openIdConnect` security scheme pointing at the discovery document.
    pub fn security_scheme(&self) -> SecurityScheme {
        SecurityScheme::open_id_connect(&self.inner.discovery_url)
    }

    /// Verify `token` (the part after `Bearer `) and return its claims.
    ///
    /// What [`Jwt`] does before a handler runs, for code that gets a token some other way.
    pub async fn verify(
        &self,
        token: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, TokenError> {
        verify::verify(&self.inner, token).await
    }
}

/// A verifier being configured; await it to fetch the discovery document and the keys.
///
/// Returned by [`Oidc::discover`].
#[must_use = "a Discover does nothing until it is awaited"]
#[derive(Debug, Clone)]
pub struct Discover {
    url: String,
    audiences: Vec<String>,
    clients: Vec<String>,
    leeway: Duration,
    min_refresh: Duration,
}

impl Discover {
    /// Accept only tokens whose `aud` contains one of `audiences`. Not called: `aud` is not
    /// checked, and a token that carries one is accepted.
    pub fn audiences<I>(mut self, audiences: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        self.audiences = audiences.into_iter().map(Into::into).collect();
        self
    }

    /// Accept only tokens issued to one of `clients`: the first present of `azp`, `client_id`
    /// (RFC 9068) and `appid` (Entra ID v1) must be in the list. Not called: not checked.
    pub fn clients<I>(mut self, clients: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        self.clients = clients.into_iter().map(Into::into).collect();
        self
    }

    /// Clock skew tolerated on `exp` and `nbf` ([`DEFAULT_LEEWAY`], 30 s, by default).
    pub fn leeway(mut self, leeway: Duration) -> Self {
        self.leeway = leeway;
        self
    }

    /// The shortest interval between two fetches of the key set triggered by an unknown `kid`
    /// ([`DEFAULT_MIN_REFRESH`], 60 s, by default).
    pub fn min_refresh(mut self, min_refresh: Duration) -> Self {
        self.min_refresh = min_refresh;
        self
    }

    async fn run(self) -> Result<Oidc, Error> {
        // jsonwebtoken picks its crypto backend from its features and panics at the first
        // verification when a dependency graph enables both. Name ours; if the application
        // installed another one first, that one stays.
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
        let url = keys::checked_url(&self.url)?;
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(Error::Client)?;
        let document: keys::Discovery = keys::fetch_json(&client, url.clone()).await?.0;
        keys::check_issuer(&self.url, &document.issuer)?;
        let algorithms = keys::algorithms(&document)?;
        let jwks_uri = keys::checked_url(&document.jwks_uri)?;
        let keys = KeyCache::fetch(client, jwks_uri, self.min_refresh).await?;
        Ok(Oidc {
            inner: Arc::new(Inner {
                discovery_url: self.url,
                issuer: document.issuer,
                audiences: self.audiences,
                clients: self.clients,
                leeway: self.leeway,
                algorithms,
                keys,
            }),
        })
    }
}

impl std::future::IntoFuture for Discover {
    type Output = Result<Oidc, Error>;
    type IntoFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

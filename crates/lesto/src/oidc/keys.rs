//! The discovery document, the key set, and the cache that keeps the keys between requests.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{Jwk, KeyAlgorithm, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey};
use reqwest::Url;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::Error;

/// The members of the discovery document lesto reads.
#[derive(Deserialize)]
pub(super) struct Discovery {
    pub(super) issuer: String,
    pub(super) jwks_uri: String,
    #[serde(default)]
    pub(super) id_token_signing_alg_values_supported: Option<Vec<String>>,
}

/// `https`, or `http` on a loopback host.
pub(super) fn checked_url(url: &str) -> Result<Url, Error> {
    let invalid = |reason| Error::Url {
        url: url.to_string(),
        reason,
    };
    let parsed = Url::parse(url).map_err(|_| invalid("not a URL"))?;
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if parsed.host_str().is_some_and(loopback) => Ok(parsed),
        "http" => Err(invalid(
            "plain http is accepted on localhost only; use https",
        )),
        _ => Err(invalid("the scheme must be https")),
    }
}

fn loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

const WELL_KNOWN: &str = "/.well-known/openid-configuration";

/// OpenID Connect Discovery 1.0, 4.3: the `issuer` in the document must be the URL the document
/// was found under. Checked when the configured URL is a well-known one; any other URL is taken
/// to be the operator's deliberate choice.
pub(super) fn check_issuer(discovery_url: &str, issuer: &str) -> Result<(), Error> {
    let Some(base) = discovery_url.strip_suffix(WELL_KNOWN) else {
        return Ok(());
    };
    if base.trim_end_matches('/') == issuer.trim_end_matches('/') {
        Ok(())
    } else {
        Err(Error::IssuerMismatch {
            url: discovery_url.to_string(),
            issuer: issuer.to_string(),
        })
    }
}

/// The algorithms lesto verifies: asymmetric only. `HS*` would let anyone holding the public key
/// sign a token (the key is used as the HMAC secret), and `none` signs nothing.
fn asymmetric(name: &str) -> Option<Algorithm> {
    let alg: Algorithm = name.parse().ok()?;
    match alg {
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => None,
        alg => Some(alg),
    }
}

/// The provider's announced algorithms that lesto verifies. The member is required by the
/// specification, which also makes `RS256` mandatory, so a document without it gets `RS256`.
pub(super) fn algorithms(document: &Discovery) -> Result<Vec<Algorithm>, Error> {
    let Some(announced) = &document.id_token_signing_alg_values_supported else {
        return Ok(vec![Algorithm::RS256]);
    };
    let algorithms: Vec<_> = announced.iter().filter_map(|a| asymmetric(a)).collect();
    if algorithms.is_empty() {
        return Err(Error::NoAlgorithm {
            announced: announced.clone(),
        });
    }
    Ok(algorithms)
}

/// GET `url` and parse the JSON body, with the `max-age` of its `Cache-Control`, if any.
pub(super) async fn fetch_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: Url,
) -> Result<(T, Option<Duration>), Error> {
    let fetch = |source| Error::Fetch {
        url: url.to_string(),
        source,
    };
    let response = client
        .get(url.clone())
        .header(http::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(fetch)?;
    if response.status() != http::StatusCode::OK {
        return Err(Error::Status {
            url: url.to_string(),
            status: response.status(),
        });
    }
    let max_age = response
        .headers()
        .get(http::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .and_then(max_age);
    let body = response.bytes().await.map_err(fetch)?;
    let value = serde_json::from_slice(&body).map_err(|source| Error::Parse {
        url: url.to_string(),
        source,
    })?;
    Ok((value, max_age))
}

/// `max-age=N` out of a `Cache-Control` value; `no-cache` / `no-store` count as zero.
fn max_age(value: &str) -> Option<Duration> {
    value.split(',').map(str::trim).find_map(|directive| {
        if directive.eq_ignore_ascii_case("no-cache") || directive.eq_ignore_ascii_case("no-store")
        {
            return Some(Duration::ZERO);
        }
        let (name, seconds) = directive.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("max-age")
            .then(|| seconds.trim().trim_matches('"').parse().ok())
            .flatten()
            .map(Duration::from_secs)
    })
}

/// A verification key, and the algorithm its JWK restricts it to, if any.
#[derive(Clone)]
pub(super) struct Key {
    pub(super) decoding: DecodingKey,
    pub(super) algorithm: Option<Algorithm>,
}

/// One fetched key set.
struct Keys {
    by_kid: HashMap<String, Key>,
    /// Keys published without a `kid`: usable only when the set has no other key.
    anonymous: Vec<Key>,
    fetched: Instant,
    max_age: Option<Duration>,
}

impl Keys {
    fn find(&self, kid: Option<&str>) -> Option<Key> {
        match kid {
            Some(kid) => self.by_kid.get(kid).cloned(),
            // A token without `kid` is unambiguous only against a set of one key.
            None => match (self.by_kid.len(), self.anonymous.as_slice()) {
                (0, [key]) => Some(key.clone()),
                (1, []) => self.by_kid.values().next().cloned(),
                _ => None,
            },
        }
    }

    fn stale(&self) -> bool {
        self.max_age
            .is_some_and(|max_age| self.fetched.elapsed() >= max_age)
    }
}

/// The JWK members lesto needs to decide whether a key verifies signatures, read before the
/// full parse so an encryption key or an unknown key type is skipped rather than failing the set.
#[derive(Deserialize)]
struct RawSet {
    keys: Vec<serde_json::Value>,
}

fn parse(set: RawSet, fetched: Instant, max_age: Option<Duration>) -> Keys {
    let mut keys = Keys {
        by_kid: HashMap::new(),
        anonymous: Vec::new(),
        fetched,
        max_age,
    };
    for value in set.keys {
        let jwk: Jwk = match serde_json::from_value(value) {
            Ok(jwk) => jwk,
            Err(error) => {
                tracing::debug!(%error, "skipping a JWK lesto cannot read");
                continue;
            }
        };
        if matches!(
            jwk.common.public_key_use,
            Some(PublicKeyUse::Encryption | PublicKeyUse::Other(_))
        ) {
            continue;
        }
        let algorithm = match jwk.common.key_algorithm {
            None => None,
            Some(alg) => match key_algorithm(alg) {
                Some(alg) => Some(alg),
                None => continue,
            },
        };
        let Ok(decoding) = DecodingKey::from_jwk(&jwk) else {
            continue;
        };
        if decoding.family() == jsonwebtoken::AlgorithmFamily::Hmac {
            continue;
        }
        let key = Key {
            decoding,
            algorithm,
        };
        match jwk.common.key_id {
            Some(kid) => {
                keys.by_kid.insert(kid, key);
            }
            None => keys.anonymous.push(key),
        }
    }
    keys
}

/// The signing algorithm a JWK's `alg` names; `None` for an encryption or symmetric one.
fn key_algorithm(alg: KeyAlgorithm) -> Option<Algorithm> {
    asymmetric(&alg.to_string())
}

/// The keys of one provider, shared by every request.
///
/// The hot path takes a read lock and clones one key. A token naming a `kid` the cache does not
/// hold, or a set past its `max-age`, triggers a fetch, at most one per `min_refresh`: the
/// `refresh` mutex serializes them, and a request that waited on it looks the key up again
/// instead of fetching a second time.
pub(super) struct KeyCache {
    client: reqwest::Client,
    url: Url,
    min_refresh: Duration,
    keys: RwLock<Arc<Keys>>,
    /// When the last fetch was attempted, successful or not.
    refresh: tokio::sync::Mutex<Instant>,
}

impl KeyCache {
    /// Fetch the set for the first time. An error, or a set with no usable key, fails startup.
    pub(super) async fn fetch(
        client: reqwest::Client,
        url: Url,
        min_refresh: Duration,
    ) -> Result<Self, Error> {
        let started = Instant::now();
        let (set, max_age) = fetch_json(&client, url.clone()).await?;
        let keys = parse(set, started, max_age);
        if keys.by_kid.is_empty() && keys.anonymous.is_empty() {
            return Err(Error::NoKeys {
                url: url.to_string(),
            });
        }
        Ok(KeyCache {
            client,
            url,
            min_refresh,
            keys: RwLock::new(Arc::new(keys)),
            refresh: tokio::sync::Mutex::new(started),
        })
    }

    fn current(&self) -> Arc<Keys> {
        // A poisoned lock still holds a complete set: the writer only swaps an `Arc`.
        self.keys
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The key for `kid`, fetching the set again if it is unknown or the set is stale.
    pub(super) async fn find(&self, kid: Option<&str>) -> Option<Key> {
        let keys = self.current();
        match keys.find(kid) {
            Some(key) if !keys.stale() => Some(key),
            // Stale: refresh if nobody else is, and verify with the key at hand meanwhile.
            Some(key) => {
                if let Ok(last) = self.refresh.try_lock() {
                    self.refresh(&keys, last).await;
                }
                self.current().find(kid).or(Some(key))
            }
            // Unknown: wait for a refresh in progress, which may bring the key.
            None => {
                let last = self.refresh.lock().await;
                self.refresh(&keys, last).await;
                self.current().find(kid)
            }
        }
    }

    /// Fetch the set again, unless another request did since `seen` was read or the last
    /// attempt is more recent than `min_refresh`.
    async fn refresh(&self, seen: &Arc<Keys>, mut last: tokio::sync::MutexGuard<'_, Instant>) {
        if !Arc::ptr_eq(seen, &self.current()) || last.elapsed() < self.min_refresh {
            return;
        }
        *last = Instant::now();
        match fetch_json::<RawSet>(&self.client, self.url.clone()).await {
            Ok((set, max_age)) => {
                let keys = parse(set, *last, max_age);
                if keys.by_kid.is_empty() && keys.anonymous.is_empty() {
                    tracing::warn!(url = %self.url, "the key set holds no signing key; keeping the previous keys");
                    return;
                }
                *self
                    .keys
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(keys);
            }
            Err(error) => {
                tracing::warn!(%error, "cannot fetch the key set; keeping the previous keys");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_age_is_read_from_cache_control() {
        assert_eq!(
            max_age("public, max-age=3600"),
            Some(Duration::from_secs(3600))
        );
        assert_eq!(max_age("no-cache"), Some(Duration::ZERO));
        assert_eq!(max_age("public"), None);
        assert_eq!(max_age("max-age=abc"), None);
    }

    #[test]
    fn plain_http_only_on_loopback() {
        assert!(checked_url("https://login.example.com/x").is_ok());
        assert!(checked_url("http://localhost:8080/x").is_ok());
        assert!(checked_url("http://127.0.0.1:8080/x").is_ok());
        assert!(checked_url("http://[::1]:8080/x").is_ok());
        assert!(checked_url("http://login.example.com/x").is_err());
        assert!(checked_url("ftp://login.example.com/x").is_err());
        assert!(checked_url("not a url").is_err());
    }

    #[test]
    fn issuer_must_match_the_well_known_url() {
        let url = "https://login.example.com/realms/a/.well-known/openid-configuration";
        assert!(check_issuer(url, "https://login.example.com/realms/a").is_ok());
        assert!(check_issuer(url, "https://login.example.com/realms/a/").is_ok());
        assert!(check_issuer(url, "https://evil.example.com/realms/a").is_err());
        assert!(check_issuer("https://login.example.com/custom", "https://x").is_ok());
    }

    #[test]
    fn symmetric_algorithms_and_none_are_refused() {
        let document = |algs: &[&str]| Discovery {
            issuer: String::new(),
            jwks_uri: String::new(),
            id_token_signing_alg_values_supported: Some(
                algs.iter().map(|a| a.to_string()).collect(),
            ),
        };
        assert_eq!(
            algorithms(&document(&["RS256", "HS256", "none", "ES256"])).unwrap(),
            vec![Algorithm::RS256, Algorithm::ES256]
        );
        assert!(algorithms(&document(&["HS256", "none"])).is_err());
    }
}

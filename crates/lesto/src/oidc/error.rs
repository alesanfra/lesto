//! Why a verifier could not be built.

/// Why [`Oidc::discover`](super::Oidc::discover) or [`Oidc::from_env`](super::Oidc::from_env)
/// failed. Configuration and startup errors only: a token that fails verification is a
/// [`TokenError`](super::TokenError), answered as a `401`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A URL is malformed, or not `https` (plain `http` is accepted on loopback only).
    Url {
        /// The URL as given.
        url: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The HTTP client could not be built (the TLS configuration, typically).
    Client(reqwest::Error),
    /// The request failed: connection, TLS, timeout.
    Fetch {
        /// What was requested.
        url: String,
        /// The client's error.
        source: reqwest::Error,
    },
    /// The provider answered with a status other than `200`.
    Status {
        /// What was requested.
        url: String,
        /// What the provider answered.
        status: http::StatusCode,
    },
    /// The body is not the JSON document expected.
    Parse {
        /// What was requested.
        url: String,
        /// The parser's error.
        source: serde_json::Error,
    },
    /// The discovery document names an issuer other than the one its URL belongs to
    /// (OpenID Connect Discovery 1.0, section 4.3).
    IssuerMismatch {
        /// The discovery URL, as configured.
        url: String,
        /// The `issuer` the document announces.
        issuer: String,
    },
    /// The provider announces no asymmetric signing algorithm lesto verifies.
    NoAlgorithm {
        /// `id_token_signing_alg_values_supported`, as announced.
        announced: Vec<String>,
    },
    /// The key set holds no key lesto can verify a signature with.
    NoKeys {
        /// The `jwks_uri`.
        url: String,
    },
    /// An environment variable read by [`Oidc::from_env`](super::Oidc::from_env) has a value
    /// that cannot be used.
    Env {
        /// The variable.
        name: &'static str,
        /// What is wrong with its value.
        reason: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Url { url, reason } => write!(f, "invalid OpenID Connect URL `{url}`: {reason}"),
            Error::Client(e) => write!(f, "cannot build the HTTP client: {e}"),
            Error::Fetch { url, source } => write!(f, "cannot fetch `{url}`: {source}"),
            Error::Status { url, status } => write!(f, "`{url}` answered {status}"),
            Error::Parse { url, source } => write!(f, "cannot parse `{url}`: {source}"),
            Error::IssuerMismatch { url, issuer } => write!(
                f,
                "the discovery document at `{url}` names the issuer `{issuer}`, which is not the \
                 URL it was fetched from"
            ),
            Error::NoAlgorithm { announced } => write!(
                f,
                "the provider announces no supported signing algorithm (announced: {announced:?}; \
                 supported: RS*, PS*, ES256, ES384, EdDSA)"
            ),
            Error::NoKeys { url } => write!(f, "the key set at `{url}` holds no signing key"),
            Error::Env { name, reason } => write!(f, "{name}: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Client(e) => Some(e),
            Error::Fetch { source, .. } => Some(source),
            Error::Parse { source, .. } => Some(source),
            _ => None,
        }
    }
}

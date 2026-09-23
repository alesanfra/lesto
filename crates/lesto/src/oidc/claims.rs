//! What a verified token says.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// A type the claims of a [`Jwt`](super::Jwt) can be read into. Implemented for every
/// `DeserializeOwned` type: derive `Deserialize` on your own claims struct.
///
/// No `#[diagnostic::on_unimplemented]`: with a blanket impl rustc reports the unmet
/// `Deserialize` bound itself, whose serde message already says to derive it.
pub trait Claims: DeserializeOwned + Send + Sync + 'static {}

impl<T: DeserializeOwned + Send + Sync + 'static> Claims for T {}

/// The registered claims every access token carries (RFC 7519, RFC 9068), plus the rest in
/// [`extra`](Self::extra), in the order the token lists them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(crate = "crate::serde")]
#[schemars(crate = "crate::schemars")]
pub struct StandardClaims {
    /// Issuer.
    pub iss: String,
    /// Subject: who the token is about. Absent from some client-credentials tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    /// Audiences; a single string in the token becomes a one-element list.
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub aud: Vec<String>,
    /// Expiry, in seconds since the epoch.
    pub exp: u64,
    /// Not valid before, in seconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<u64>,
    /// Issued at, in seconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iat: Option<u64>,
    /// Space-separated scopes (RFC 8693, RFC 9068).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Scopes as Entra ID and Okta send them: a list, or one space-separated string.
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub scp: Vec<String>,
    /// Every other claim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl StandardClaims {
    /// The scopes granted, from `scope` or `scp`.
    pub fn scopes(&self) -> impl Iterator<Item = &str> {
        let from_scope = self.scope.iter().flat_map(|s| s.split_whitespace());
        let from_scp = self.scp.iter().flat_map(|s| s.split_whitespace());
        from_scope.chain(from_scp)
    }

    /// Does the token grant `scope`?
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes().any(|s| s == scope)
    }
}

fn one_or_many<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(crate = "crate::serde", untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(one) => vec![one],
        OneOrMany::Many(many) => many,
    })
}

/// The scopes of raw claims: `scope` (a space-separated string), then `scp` (a list or a string).
pub(super) fn scopes(claims: &Map<String, Value>) -> impl Iterator<Item = &str> {
    ["scope", "scp"]
        .into_iter()
        .filter_map(|name| claims.get(name))
        .flat_map(|value| match value {
            Value::String(s) => vec![s.as_str()],
            Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        })
        .flat_map(str::split_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_come_from_scope_and_scp() {
        let claims: StandardClaims = serde_json::from_value(serde_json::json!({
            "iss": "https://issuer", "exp": 1, "aud": "api",
            "scope": "notes:read notes:write", "scp": ["admin"], "tenant": "t1"
        }))
        .unwrap();
        assert_eq!(claims.aud, vec!["api"]);
        assert_eq!(
            claims.scopes().collect::<Vec<_>>(),
            ["notes:read", "notes:write", "admin"]
        );
        assert_eq!(claims.extra["tenant"], "t1");

        let raw = serde_json::json!({"scope": "a b", "scp": "c"});
        assert_eq!(
            scopes(raw.as_object().unwrap()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }
}

//! [`Oidc::from_env`](super::Oidc::from_env): the verifier configured by the environment.

use std::time::Duration;

use super::{Discover, Error, Oidc};

/// The discovery URL; unset, `from_env` configures nothing.
pub const DISCOVERY_URL: &str = "LESTO_OIDC_DISCOVERY_URL";
/// Accepted audiences, comma separated.
pub const AUDIENCES: &str = "LESTO_OIDC_AUDIENCES";
/// Accepted clients, comma separated.
pub const CLIENTS: &str = "LESTO_OIDC_CLIENTS";
/// Leeway on `exp` and `nbf`, in whole seconds.
pub const LEEWAY_SECS: &str = "LESTO_OIDC_LEEWAY_SECS";

/// Read the variables through `get`, so tests never write the process environment.
pub(super) fn read(get: impl Fn(&str) -> Option<String>) -> Result<Option<Discover>, Error> {
    let get = |name| get(name).filter(|value| !value.trim().is_empty());
    let Some(url) = get(DISCOVERY_URL) else {
        return Ok(None);
    };
    let list = |name| -> Vec<String> {
        get(name)
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut discover = Oidc::discover(url.trim())
        .audiences(list(AUDIENCES))
        .clients(list(CLIENTS));
    if let Some(leeway) = get(LEEWAY_SECS) {
        let seconds = leeway.trim().parse().map_err(|_| Error::Env {
            name: LEEWAY_SECS,
            reason: format!("`{leeway}` is not a whole number of seconds"),
        })?;
        discover = discover.leeway(Duration::from_secs(seconds));
    }
    Ok(Some(discover))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn read_map(vars: &[(&str, &str)]) -> Result<Option<Discover>, Error> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        read(|name| vars.get(name).cloned())
    }

    #[test]
    fn nothing_without_a_discovery_url() {
        assert!(read_map(&[]).unwrap().is_none());
        assert!(read_map(&[(DISCOVERY_URL, " ")]).unwrap().is_none());
        assert!(read_map(&[(AUDIENCES, "api")]).unwrap().is_none());
    }

    #[test]
    fn lists_and_leeway_are_parsed() {
        let discover = read_map(&[
            (
                DISCOVERY_URL,
                "https://login.example.com/.well-known/openid-configuration",
            ),
            (AUDIENCES, "api://notes, api://admin,"),
            (CLIENTS, "web"),
            (LEEWAY_SECS, "5"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            discover.url,
            "https://login.example.com/.well-known/openid-configuration"
        );
        assert_eq!(discover.audiences, ["api://notes", "api://admin"]);
        assert_eq!(discover.clients, ["web"]);
        assert_eq!(discover.leeway, Duration::from_secs(5));
    }

    #[test]
    fn a_bad_leeway_is_an_error() {
        let error = read_map(&[(DISCOVERY_URL, "https://x"), (LEEWAY_SECS, "30s")]).unwrap_err();
        assert!(matches!(
            error,
            Error::Env {
                name: LEEWAY_SECS,
                ..
            }
        ));
    }
}

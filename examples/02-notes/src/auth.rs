//! Who is calling: the `User` principal and how it is derived from a bearer token.

use std::collections::HashMap;

use lesto::db::Authenticated;
use lesto::prelude::*;

use crate::state::AppState;

/// An authenticated caller with its permissions.
#[derive(Debug, Clone)]
pub struct User {
    pub name: String,
    pub permissions: Vec<String>,
}

/// The only trait to implement for `lesto::db`: credential in, identity out.
impl Authenticated for User {
    type State = AppState;
    type Credential = Bearer;

    async fn authenticate(token: Bearer, state: &AppState) -> Result<Self, HttpError> {
        state
            .tokens
            .get(token.token())
            .cloned()
            .ok_or_else(|| HttpError::unauthorized("Unknown token"))
    }

    fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }
}

/// Who owns which token. A real application would verify a JWT or query a session table.
#[derive(Clone, Default)]
pub struct Tokens(HashMap<String, User>);

impl Tokens {
    pub fn get(&self, token: &str) -> Option<&User> {
        self.0.get(token)
    }

    /// `alice-token` may write and delete, `bob-token` may only write.
    pub fn demo() -> Self {
        let user = |name: &str, perms: &[&str]| User {
            name: name.to_string(),
            permissions: perms.iter().map(|p| p.to_string()).collect(),
        };
        Tokens(HashMap::from([
            (
                "alice-token".to_string(),
                user("alice", &["notes:write", "notes:delete"]),
            ),
            ("bob-token".to_string(), user("bob", &["notes:write"])),
        ]))
    }
}

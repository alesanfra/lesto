// Chapters 8 and 9: a custom extractor and the security schemes
use lesto::axum::extract::FromRequestParts;
use lesto::http::request::Parts;
use lesto::prelude::*;
use lesto::{BearerAuth, OperationBuilder, OperationInput, Rejection};

#[lesto::model]
#[derive(Clone)]
pub struct User {
    pub id: u64,
    pub name: String,
}

/// The authenticated user.
pub struct CurrentUser(pub User);

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = Rejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Rejection> {
        let auth = Bearer::<BearerAuth>::from_request_parts(parts, state).await?;
        match auth.token() {
            "secret" => Ok(CurrentUser(User {
                id: 1,
                name: "Ada".into(),
            })),
            _ => Err(HttpError::unauthorized("invalid token").into()),
        }
    }
}

impl OperationInput for CurrentUser {
    fn describe(builder: &mut OperationBuilder<'_>) {
        Bearer::<BearerAuth>::describe(builder);
    }
}

pub struct AdminKey;

impl ApiKeyScheme for AdminKey {
    const NAME: &'static str = "adminKey";
    const KEY: &'static str = "X-Admin-Key";
}

pub struct Session;

impl ApiKeyScheme for Session {
    const NAME: &'static str = "session";
    const KEY: &'static str = "sid";
    const LOCATION: ApiKeyIn = ApiKeyIn::Cookie;
}

pub struct OAuth2;

impl AuthScheme for OAuth2 {
    const NAME: &'static str = "oauth2";

    fn scheme() -> SecurityScheme {
        SecurityScheme::oauth2(
            OAuthFlows::password("/token")
                .scope("items:read", "Read items")
                .scope("items:write", "Create and modify items"),
        )
    }

    fn scopes() -> &'static [&'static str] {
        &["items:read"]
    }
}

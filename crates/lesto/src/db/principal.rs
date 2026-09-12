//! Who holds a store.
//!
//! Users implement [`Authenticated`] once for their user type. [`Public`] is the anonymous
//! principal. Both then satisfy [`Principal`], which is what stores need.

use std::future::Future;

use crate::axum::extract::FromRequestParts;
use crate::axum::response::{IntoResponse, Response};
use crate::http::request::Parts;
use crate::{HttpError, OperationBuilder, OperationInput};

/// An identity derived from a credential. The one trait to implement.
///
/// ```ignore
/// impl Authenticated for User {
///     type State = AppState;
///     type Credential = Bearer;
///     async fn authenticate(token: Bearer, state: &AppState) -> Result<Self, HttpError> {
///         state.tokens.verify(token.token())
///     }
///     fn has_permission(&self, permission: &str) -> bool {
///         self.scopes.iter().any(|s| s == permission)
///     }
/// }
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot hold permissions",
    label = "not an authenticated principal",
    note = "a permission string such as `\"notes:write\"` needs an authenticated principal: `Public` never has permissions, so pass `Anyone`, or declare the handler argument as `YourStore<_, User>` where `User: lesto::db::Authenticated`",
    note = "if `{Self}` is your own user type, implement `lesto::db::Authenticated` for it (credential + authenticate + has_permission)"
)]
pub trait Authenticated: Sized + Send + Sync + 'static {
    /// The application state, as in `App<State>`.
    type State: Send + Sync;
    /// The extractor that reads the raw credential: `Bearer`, `ApiKey<S>`, `Basic`, or any
    /// extractor that documents itself. Its 401 and security scheme are inherited.
    type Credential: FromRequestParts<Self::State> + OperationInput + Send;

    /// Turn the credential into an identity, or fail (typically with
    /// `HttpError::unauthorized`).
    fn authenticate(
        credential: Self::Credential,
        state: &Self::State,
    ) -> impl Future<Output = Result<Self, HttpError>> + Send;

    /// Does this identity hold `permission` (e.g. `"notes:write"`)?
    fn has_permission(&self, permission: &str) -> bool;
}

/// The anonymous principal: always extracted, never holds permissions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Public;

/// How a principal documents itself. Implemented for [`Public`] and every [`Authenticated`].
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a principal",
    label = "expected `lesto::db::Public` or a type implementing `lesto::db::Authenticated`",
    note = "implement `lesto::db::Authenticated` for `{Self}` (credential + authenticate + has_permission)"
)]
pub trait PrincipalDocs: Sized + Send + Sync + 'static {
    /// Add the security requirement and error responses this principal implies.
    fn describe(builder: &mut OperationBuilder<'_>);
}

/// A principal that can be extracted with state `S`. Implemented for [`Public`] (any state)
/// and for every [`Authenticated`] type (its own `State`).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a principal for state `{S}`",
    label = "cannot be extracted from an `App<{S}>` request",
    note = "if `{Self}` implements `lesto::db::Authenticated`, its `type State` must be `{S}`; otherwise implement it, or use `lesto::db::Public`"
)]
pub trait Principal<S>: PrincipalDocs {
    /// Extract the principal, answering with a full response (401 and friends) on failure.
    fn extract(parts: &mut Parts, state: &S)
    -> impl Future<Output = Result<Self, Response>> + Send;
}

impl<P: Authenticated> PrincipalDocs for P {
    fn describe(builder: &mut OperationBuilder<'_>) {
        P::Credential::describe(builder);
        builder.error_response(403, "Forbidden");
    }
}

impl<P: Authenticated> Principal<P::State> for P {
    async fn extract(parts: &mut Parts, state: &P::State) -> Result<Self, Response> {
        let credential = P::Credential::from_request_parts(parts, state)
            .await
            .map_err(IntoResponse::into_response)?;
        P::authenticate(credential, state)
            .await
            .map_err(IntoResponse::into_response)
    }
}

impl PrincipalDocs for Public {
    fn describe(_builder: &mut OperationBuilder<'_>) {}
}

impl<S: Send + Sync> Principal<S> for Public {
    async fn extract(_parts: &mut Parts, _state: &S) -> Result<Self, Response> {
        Ok(Public)
    }
}

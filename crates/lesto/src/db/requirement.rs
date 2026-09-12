//! What a store method demands of the principal before it runs.

use crate::db::error::Error;
use crate::db::principal::Authenticated;

/// A precondition checked against the principal before a transaction opens.
///
/// Built-in: [`Anyone`] (no check) and a `&'static str` permission (`"notes:write"`), which
/// needs an [`Authenticated`] principal. Implement the trait for your own combinators.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a requirement for principal `{P}`",
    label = "expected `Anyone` or a permission string",
    note = "pass `lesto::db::Anyone`, a `&'static str` permission such as `\"notes:write\"`, or a type implementing `lesto::db::Requirement<{P}>`"
)]
pub trait Requirement<P> {
    /// `Ok(())` to proceed, or the error to answer with (usually `Error::Forbidden`).
    fn check(&self, principal: &P) -> Result<(), Error>;
}

/// No requirement: the method runs for any principal, `Public` included.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Anyone;

impl<P> Requirement<P> for Anyone {
    fn check(&self, _principal: &P) -> Result<(), Error> {
        Ok(())
    }
}

impl<P: Authenticated> Requirement<P> for &'static str {
    fn check(&self, principal: &P) -> Result<(), Error> {
        if principal.has_permission(self) {
            Ok(())
        } else {
            Err(Error::Forbidden { permission: self })
        }
    }
}

//! Settings published to the database at the start of every store transaction.
//!
//! A principal declares them once, in [`Authenticated::transaction_settings`], and every
//! transaction that principal opens carries them — which is how a row level security policy
//! gets to know who is asking:
//!
//! ```ignore
//! impl Authenticated for User {
//!     // ...credential, authenticate, has_permission...
//!
//!     fn transaction_settings(&self) -> TransactionSettings {
//!         TransactionSettings::empty()
//!             .set("app.user_id", self.id)
//!             .set_opt("app.organization_id", self.organization_id)
//!             .set("app.is_staff", self.is_staff)
//!     }
//! }
//! ```
//!
//! On Postgres that becomes one `SET LOCAL` per entry, appended to the `BEGIN` of the
//! transaction and sent in the same round trip. `SET LOCAL` lasts until the transaction ends,
//! so nothing stays behind on the pooled connection. Read them back with
//! `current_setting('app.user_id', true)` — and `nullif(current_setting(..., true), '')` for a
//! value set through [`TransactionSettings::set_opt`], whose `None` is the empty string.
//!
//! [`Authenticated::transaction_settings`]: crate::db::Authenticated::transaction_settings
//!
//! # Why the values are a closed set
//!
//! `SET LOCAL` takes no bind parameters, on any database. Every value here is therefore
//! interpolated into SQL, and the type is the only thing standing between a principal field and
//! an injection. So [`Literal`] holds a `bool`, an integer or a [`Uuid`](uuid::Uuid) and
//! nothing else: every one of those has a text form drawn from a closed alphabet that cannot
//! contain a quote. There is deliberately no text variant — if you need one, the question to
//! answer first is where the text comes from.
//!
//! Setting *names* are `&'static str` for the same reason: a name comes from the source, never
//! from a request. Their characters are checked anyway, and a rejected name fails the request
//! with a 500 rather than reaching the database.

use std::fmt::Write as _;

use crate::db::error::Error;

/// The settings one principal publishes to each of its transactions.
///
/// Build it with [`empty`](Self::empty) and [`set`](Self::set); see the [module
/// docs](self) for what it renders to and why the value types are restricted.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransactionSettings {
    entries: Vec<(&'static str, Option<Literal>)>,
    /// The first name that failed [`is_valid_name`], kept to fail the request instead of
    /// quietly dropping a setting a security policy may depend on.
    rejected: Option<&'static str>,
}

impl TransactionSettings {
    /// No settings: the transaction opens with a bare `BEGIN`, exactly as it would without
    /// this feature.
    pub const fn empty() -> Self {
        Self {
            entries: Vec::new(),
            rejected: None,
        }
    }

    /// Publish `value` under `name`.
    ///
    /// `name` must be lowercase ASCII letters, digits, `_` and `.`, starting with a letter.
    /// Postgres only accepts a custom setting with a dotted prefix (`app.user_id`), so use
    /// one; an undotted name is rejected by the database itself with `unrecognized
    /// configuration parameter`.
    #[must_use]
    pub fn set(mut self, name: &'static str, value: impl Into<Literal>) -> Self {
        self.push(name, Some(value.into()));
        self
    }

    /// Publish an optional value: `None` becomes the empty string, so read it back with
    /// `nullif(current_setting('name', true), '')`.
    ///
    /// A missing setting and one set to the empty string are not the same thing to
    /// `current_setting`, and an absent entry would make a policy fall back to whatever it
    /// does for an unset parameter. Emitting the empty string keeps the shape of the
    /// transaction the same whether the value is there or not.
    #[must_use]
    pub fn set_opt(mut self, name: &'static str, value: Option<impl Into<Literal>>) -> Self {
        self.push(name, value.map(Into::into));
        self
    }

    /// Are there no settings to publish?
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many settings are published.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn push(&mut self, name: &'static str, value: Option<Literal>) {
        if !is_valid_name(name) {
            debug_assert!(
                false,
                "`{name}` is not a valid setting name: expected lowercase ASCII letters, \
                 digits, `_` and `.`, starting with a letter"
            );
            self.rejected = self.rejected.or(Some(name));
            return;
        }
        self.entries.push((name, value));
    }

    /// The entries, or the error the request fails with when a name was rejected.
    ///
    /// A rejected name is a bug in the `transaction_settings` implementation, not something a
    /// request can cause, so it answers 500 — and it answers rather than panicking because a
    /// store method has no business unwinding. Dropping the setting and carrying on is the one
    /// option that is not available: a policy that reads it would then silently see nothing.
    pub(crate) fn entries(&self) -> Result<&[(&'static str, Option<Literal>)], Error> {
        match self.rejected {
            None => Ok(&self.entries),
            Some(name) => Err(Error::internal(InvalidName(name))),
        }
    }
}

/// A value that can be published as a transaction setting.
///
/// Every variant has a text form drawn from a closed alphabet — hex digits, decimal digits,
/// `-`, or `true`/`false` — which is what makes interpolating it into `SET LOCAL` safe. There
/// is no text variant, on purpose: see the [module docs](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Literal {
    /// `true` or `false`.
    Bool(bool),
    /// A decimal integer.
    Int(i64),
    /// A UUID in its hyphenated form.
    Uuid(uuid::Uuid),
}

impl Literal {
    /// Append the text form, without quotes. The caller quotes.
    ///
    /// Postgres is the only dialect that renders settings, so with `db` alone — or with only
    /// MySQL or SQLite — nothing calls this.
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    pub(crate) fn render_into(&self, out: &mut String) {
        match self {
            Literal::Bool(true) => out.push_str("true"),
            Literal::Bool(false) => out.push_str("false"),
            // Infallible: writing into a `String` cannot fail.
            Literal::Int(value) => {
                let _ = write!(out, "{value}");
            }
            Literal::Uuid(value) => {
                let _ = write!(out, "{}", value.as_hyphenated());
            }
        }
    }
}

impl From<bool> for Literal {
    fn from(value: bool) -> Self {
        Literal::Bool(value)
    }
}

impl From<i64> for Literal {
    fn from(value: i64) -> Self {
        Literal::Int(value)
    }
}

impl From<i32> for Literal {
    fn from(value: i32) -> Self {
        Literal::Int(value.into())
    }
}

impl From<uuid::Uuid> for Literal {
    fn from(value: uuid::Uuid) -> Self {
        Literal::Uuid(value)
    }
}

/// Lowercase ASCII letters, digits, `_` and `.`, starting with a letter.
///
/// Deliberately narrower than what Postgres accepts: a name is written in the source, so there
/// is no cost to keeping the alphabet small, and a small alphabet is what makes the quoting
/// below provably safe.
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// The cause logged when a setting name was rejected. Carries the name; the response is a bare
/// 500 like every other internal error.
#[derive(Debug)]
struct InvalidName(&'static str);

impl std::fmt::Display for InvalidName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` is not a valid transaction setting name: expected lowercase ASCII letters, \
             digits, `_` and `.`, starting with a letter",
            self.0
        )
    }
}

impl std::error::Error for InvalidName {}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(value: Literal) -> String {
        let mut out = String::new();
        value.render_into(&mut out);
        out
    }

    #[test]
    fn empty_is_empty() {
        assert!(TransactionSettings::empty().is_empty());
        assert_eq!(TransactionSettings::empty().len(), 0);
    }

    #[test]
    fn set_keeps_insertion_order() {
        let settings = TransactionSettings::empty()
            .set("app.b", 2i64)
            .set("app.a", 1i64);
        let entries = settings.entries().unwrap();
        assert_eq!(entries[0].0, "app.b");
        assert_eq!(entries[1].0, "app.a");
    }

    #[test]
    fn set_opt_none_is_an_entry_with_no_value() {
        let settings = TransactionSettings::empty().set_opt("app.org", None::<i64>);
        let entries = settings.entries().unwrap();
        assert_eq!(entries, [("app.org", None)]);
    }

    #[test]
    fn literals_render_without_quotes() {
        assert_eq!(render(Literal::Bool(true)), "true");
        assert_eq!(render(Literal::Bool(false)), "false");
        assert_eq!(render(Literal::Int(-42)), "-42");
        assert_eq!(render(Literal::Int(i64::MIN)), i64::MIN.to_string());
        assert_eq!(
            render(Literal::Uuid(uuid::Uuid::nil())),
            "00000000-0000-0000-0000-000000000000"
        );
    }

    /// The guarantee the whole design rests on: nothing a `Literal` can render can end the
    /// quoted string it is interpolated into. If a variant is ever added, this fails.
    #[test]
    fn every_rendered_char_is_safe() {
        let mut samples = vec![
            Literal::Bool(true),
            Literal::Bool(false),
            Literal::Int(0),
            Literal::Int(i64::MIN),
            Literal::Int(i64::MAX),
            Literal::Uuid(uuid::Uuid::nil()),
            Literal::Uuid(uuid::Uuid::max()),
        ];
        for byte in 0..=u8::MAX {
            samples.push(Literal::Int(i64::from(byte)));
            samples.push(Literal::Uuid(uuid::Uuid::from_bytes([byte; 16])));
        }
        for sample in samples {
            let rendered = render(sample);
            assert!(
                rendered
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "{sample:?} rendered as {rendered:?}, which leaves the safe alphabet"
            );
        }
    }

    #[test]
    fn valid_names_are_accepted() {
        for name in ["app.user_id", "a", "app.v2.tenant", "app_user"] {
            assert!(is_valid_name(name), "{name} should be valid");
        }
    }

    #[test]
    fn invalid_names_are_rejected() {
        for name in [
            "",
            "1app.user",
            ".app",
            "_app",
            "App.user",
            "app.user id",
            "app.user'",
            "app.user;DROP",
            "app.user\n",
            "app.üser",
        ] {
            assert!(!is_valid_name(name), "{name} should be rejected");
        }
    }

    /// A rejected name fails the request, and does not silently vanish.
    ///
    /// Built by hand rather than through `set`, which fires a `debug_assert` first: the assert
    /// is for the author running tests, this is the behavior in release.
    #[test]
    fn a_rejected_name_becomes_an_error() {
        let settings = TransactionSettings {
            entries: vec![("app.user_id", Some(Literal::Int(1)))],
            rejected: Some("app.user;DROP TABLE notes"),
        };
        let error = settings.entries().unwrap_err();
        assert_eq!(
            error.status(),
            crate::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(error.to_string().contains("app.user;DROP TABLE notes"));
    }

    /// The name is only ever `&'static str`, but check the skip anyway: a dropped setting must
    /// not reach the database as if it had been published.
    #[test]
    fn a_rejected_name_does_not_become_an_entry() {
        let mut settings = TransactionSettings::empty();
        settings.rejected = Some("already bad");
        settings.entries.push(("app.user_id", None));
        assert_eq!(settings.len(), 1);
        assert!(settings.entries().is_err());
    }
}

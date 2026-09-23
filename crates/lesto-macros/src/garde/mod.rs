// Vendored from garde_derive 0.23.0 (https://github.com/jprochazk/garde), licensed MIT OR
// Apache-2.0 and used here under Apache-2.0. Modified by lesto: only the `Validate` derive is
// kept (not `select!`), and it is an ordinary function called by `#[lesto::model]`. See
// ./NOTICE.md.

// Vendored code: kept as close to upstream as possible, lints included.
#![allow(clippy::all, clippy::pedantic)]

mod check;
mod emit;
mod model;
mod syntax;
mod util;

use proc_macro2::TokenStream;
use syn::DeriveInput;

/// `#[derive(garde::Validate)]`, emitting paths through `::lesto::garde`.
pub fn derive_validate(input: DeriveInput) -> TokenStream {
    let input = match syntax::parse(input) {
        Ok(v) => v,
        Err(e) => return e.into_compile_error(),
    };
    let input = match check::check(input) {
        Ok(v) => v,
        Err(e) => return e.into_compile_error(),
    };
    emit::emit(input)
}

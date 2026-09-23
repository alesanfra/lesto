// Vendored from garde_derive 0.23.0 (https://github.com/jprochazk/garde), licensed MIT OR
// Apache-2.0 and used here under Apache-2.0. Modified by lesto: generated paths name
// `::lesto::garde` instead of `::garde`, and `crate::` imports point into this module. See
// ./NOTICE.md.

pub trait MaybeFoldError {
    fn maybe_fold(&mut self, error: syn::Error);
}

impl MaybeFoldError for Option<syn::Error> {
    fn maybe_fold(&mut self, error: syn::Error) {
        match self {
            Some(v) => {
                v.combine(error);
            }
            None => *self = Some(error),
        }
    }
}

pub fn default_ctx_name() -> syn::Ident {
    syn::Ident::new("__garde_user_ctx", proc_macro2::Span::call_site())
}

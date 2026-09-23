//! Attribute macros for `lesto` routes.
//!
//! ```ignore
//! /// Summary line.
//! ///
//! /// Longer description in Markdown.
//! #[lesto::post("/users", status = 201, tag = "users", responses(404, 409), deprecated)]
//! async fn create_user(Json(body): Json<CreateUser>) -> Json<User> { .. }
//! ```
//!
//! The function is left untouched. Next to it the macro emits a braced struct with the same
//! name (`struct create_user {}`); a braced struct lives only in the type namespace, so it
//! does not collide with the function, and `routes![create_user]` can reach both.
#![warn(missing_docs)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Expr, ExprLit, FnArg, GenericArgument, Ident, ItemFn, Lit, LitInt, LitStr, Meta, PathArguments,
    Token, Type,
};

struct RouteArgs {
    path: LitStr,
    status: Option<LitInt>,
    tags: Vec<LitStr>,
    summary: Option<LitStr>,
    description: Option<LitStr>,
    operation_id: Option<LitStr>,
    deprecated: bool,
    responses: Vec<LitInt>,
    security: Vec<(LitStr, Vec<LitStr>)>,
    public: bool,
    /// App state type used for the compile-time extractor checks (default: inferred from
    /// a `State<T>` argument, else `()`).
    state: Option<Type>,
}

/// One item of `security(..)`: `"name"` or `name = ["scope", ..]` / `"name" = ["scope"]`.
struct SecurityItem {
    name: LitStr,
    scopes: Vec<LitStr>,
}

impl Parse for SecurityItem {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: LitStr = if input.peek(LitStr) {
            input.parse()?
        } else {
            let ident: Ident = input.parse()?;
            LitStr::new(&ident.to_string(), ident.span())
        };
        let mut scopes = Vec::new();
        if input.peek(Token![=]) {
            input.parse::<Token![=]>()?;
            let content;
            syn::bracketed!(content in input);
            scopes = Punctuated::<LitStr, Token![,]>::parse_terminated(&content)?
                .into_iter()
                .collect();
        }
        Ok(SecurityItem { name, scopes })
    }
}

impl Parse for RouteArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let path: LitStr = input.parse().map_err(|e| {
            syn::Error::new(
                e.span(),
                "expected a path string literal, e.g. #[lesto::get(\"/users/{id}\")]",
            )
        })?;
        let mut args = RouteArgs {
            path,
            status: None,
            tags: Vec::new(),
            summary: None,
            description: None,
            operation_id: None,
            deprecated: false,
            responses: Vec::new(),
            security: Vec::new(),
            public: false,
            state: None,
        };
        if input.is_empty() {
            return Ok(args);
        }
        input.parse::<Token![,]>()?;
        let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
        for meta in metas {
            match meta {
                Meta::Path(p) if p.is_ident("deprecated") => args.deprecated = true,
                Meta::Path(p) if p.is_ident("public") => args.public = true,
                Meta::NameValue(nv) => {
                    let name = nv
                        .path
                        .get_ident()
                        .map(Ident::to_string)
                        .unwrap_or_default();
                    match name.as_str() {
                        "status" => args.status = Some(expect_int(&nv.value)?),
                        "tag" => args.tags.push(expect_str(&nv.value)?),
                        "summary" => args.summary = Some(expect_str(&nv.value)?),
                        "description" => args.description = Some(expect_str(&nv.value)?),
                        "operation_id" => args.operation_id = Some(expect_str(&nv.value)?),
                        "state" => {
                            let ty: Type =
                                syn::parse2(nv.value.to_token_stream()).map_err(|_| {
                                    syn::Error::new_spanned(
                                        &nv.value,
                                        "expected a type, e.g. `state = AppState`",
                                    )
                                })?;
                            args.state = Some(ty);
                        }
                        "deprecated" => {
                            args.deprecated = matches!(&nv.value, Expr::Lit(ExprLit { lit: Lit::Bool(b), .. }) if b.value)
                        }
                        _ => {
                            return Err(syn::Error::new_spanned(
                                nv.path,
                                "unknown route option; expected one of: status, tag, tags(..), summary, description, operation_id, deprecated, public, responses(..), security(..), state = T",
                            ));
                        }
                    }
                }
                Meta::List(list) => {
                    let name = list
                        .path
                        .get_ident()
                        .map(Ident::to_string)
                        .unwrap_or_default();
                    match name.as_str() {
                        "tags" => {
                            let items = list.parse_args_with(
                                Punctuated::<LitStr, Token![,]>::parse_terminated,
                            )?;
                            args.tags.extend(items);
                        }
                        "responses" => {
                            let items = list.parse_args_with(
                                Punctuated::<LitInt, Token![,]>::parse_terminated,
                            )?;
                            args.responses.extend(items);
                        }
                        "security" => {
                            let items = list.parse_args_with(
                                Punctuated::<SecurityItem, Token![,]>::parse_terminated,
                            )?;
                            args.security
                                .extend(items.into_iter().map(|i| (i.name, i.scopes)));
                        }
                        _ => {
                            return Err(syn::Error::new_spanned(
                                list.path,
                                "unknown route option; expected one of: status, tag, tags(..), summary, description, operation_id, deprecated, public, responses(..), security(..), state = T",
                            ));
                        }
                    }
                }
                other => return Err(syn::Error::new_spanned(other, "unexpected route option")),
            }
        }
        Ok(args)
    }
}

fn expect_str(expr: &Expr) -> syn::Result<LitStr> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.clone()),
        other => Err(syn::Error::new_spanned(other, "expected a string literal")),
    }
}

fn expect_int(expr: &Expr) -> syn::Result<LitInt> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => Ok(i.clone()),
        other => Err(syn::Error::new_spanned(
            other,
            "expected an integer literal",
        )),
    }
}

/// Split `///` doc comments into (summary, description) the way FastAPI treats docstrings:
/// the first paragraph is the summary, the rest is the description.
fn doc_parts(func: &ItemFn) -> (Option<String>, Option<String>) {
    let mut lines: Vec<String> = Vec::new();
    for attr in &func.attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(nv) = &attr.meta
            && let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
        {
            for line in s.value().split('\n') {
                lines.push(
                    line.strip_prefix(' ')
                        .unwrap_or(line)
                        .trim_end()
                        .to_string(),
                );
            }
        }
    }
    let text = lines.join("\n");
    let text = text.trim();
    if text.is_empty() {
        return (None, None);
    }
    let (summary, rest) = match text.find("\n\n") {
        Some(idx) => (&text[..idx], text[idx..].trim()),
        None => (text, ""),
    };
    let summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let description = if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    };
    (Some(summary), description)
}

/// `create_user` -> `Create user`, FastAPI's default summary.
fn humanize(name: &str) -> String {
    let mut s = name.replace('_', " ");
    if let Some(first) = s.get(..1) {
        let upper = first.to_uppercase();
        s.replace_range(..1, &upper);
    }
    s
}

/// `State<T>` → `Some(T)`.
fn state_arg(ty: &Type) -> Option<Type> {
    let Type::Path(p) = ty else { return None };
    let last = p.path.segments.last()?;
    if last.ident != "State" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(t) if args.args.len() == 1 => Some(t.clone()),
        _ => None,
    }
}

/// Type-level assertions that mirror axum's `Handler` requirements, one per argument, so a
/// mistake is reported on the offending argument with a lesto-specific explanation instead
/// of the opaque `Handler<_, _> is not satisfied`.
///
/// Checks that do not depend on the state type run in a plain fn next to the handler. The
/// extractor checks need the state: when it is known (a `State<T>` argument or `state = T`)
/// they run there too; otherwise they become `where` clauses of `<marker>::__lesto_check`,
/// which `routes![]` calls so the state is inferred from the `App<S>` the set is added to.
fn handler_checks(func: &ItemFn, explicit_state: Option<&Type>) -> (TokenStream2, TokenStream2) {
    let types: Vec<&Type> = func
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat) => Some(&*pat.ty),
            FnArg::Receiver(_) => None,
        })
        .collect();
    let known_state: Option<Type> = explicit_state
        .cloned()
        .or_else(|| types.iter().find_map(|t| state_arg(t)));

    let mut checks = Vec::new();
    let mut bounds = Vec::new();
    let mut needs_marker = false;
    if let Some((last, init)) = types.split_last() {
        for ty in init {
            checks.push(quote_spanned! { ty.span() =>
                ::lesto::__private::check_documented_input::<#ty>();
            });
            match &known_state {
                Some(state) => checks.push(quote_spanned! { ty.span() =>
                    ::lesto::__private::check_parts::<#ty, #state>();
                }),
                None => bounds.push(quote_spanned! { ty.span() =>
                    #ty: ::lesto::__private::PartsExtractor<__S>,
                }),
            }
        }
        checks.push(quote_spanned! { last.span() =>
            ::lesto::__private::check_documented_input::<#last>();
        });
        match &known_state {
            Some(state) => checks.push(quote_spanned! { last.span() =>
                ::lesto::__private::check_last::<#last, #state, _>();
            }),
            None => {
                needs_marker = true;
                bounds.push(quote_spanned! { last.span() =>
                    #last: ::lesto::__private::LastExtractor<__S, __M>,
                });
            }
        }
    }
    if let syn::ReturnType::Type(_, ret) = &func.sig.output {
        checks.push(quote_spanned! { ret.span() =>
            ::lesto::__private::check_response::<#ret>();
            ::lesto::__private::check_documented_output::<#ret>();
        });
    }
    let check_fn = format_ident!("__lesto_check_{}", func.sig.ident);
    let immediate = quote! {
        #[allow(dead_code, non_snake_case, unused_qualifications)]
        fn #check_fn() {
            #(#checks)*
        }
    };
    let marker = needs_marker.then(|| quote! { , __M });
    let deferred = quote! {
        #[doc(hidden)]
        #[allow(unused_qualifications)]
        pub fn __lesto_check<__S #marker>(_set: &::lesto::RouteSet<__S>)
        where
            __S: Send + Sync + 'static,
            #(#bounds)*
        {
        }
    };
    (immediate, deferred)
}

fn route(method: &str, attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match syn::parse::<RouteArgs>(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let func = match syn::parse::<ItemFn>(item) {
        Ok(f) => f,
        Err(e) => return e.to_compile_error().into(),
    };
    expand(method, args, func).into()
}

fn expand(method: &str, args: RouteArgs, func: ItemFn) -> TokenStream2 {
    let name = &func.sig.ident;
    let vis = &func.vis;
    let method_ident = format_ident!("{}", method);
    let path = &args.path;

    let (doc_summary, doc_description) = doc_parts(&func);
    let summary = args
        .summary
        .map(|s| s.value())
        .or(doc_summary)
        .unwrap_or_else(|| humanize(&name.to_string()));
    let description = args.description.map(|s| s.value()).or(doc_description);
    let name_str = name.to_string();
    let operation_id = args.operation_id.map(|s| quote! { .operation_id(#s) });

    let description = description.map(|d| quote! { .description(#d) });
    let status = args.status.map(|s| quote! { .status(#s as u16) });
    let deprecated = args.deprecated.then(|| quote! { .deprecated(true) });
    let tags = args.tags.iter().map(|t| quote! { .tag(#t) });
    let responses = args.responses.iter().map(|r| quote! { .error(#r as u16) });
    let security = args.security.iter().map(|(name, scopes)| {
        quote! { .security(#name, &[#(#scopes),*]) }
    });
    let public = args.public.then(|| quote! { .public() });
    let (checks, deferred_checks) = handler_checks(&func, args.state.as_ref());

    quote! {
        #func

        #checks

        #[allow(non_camel_case_types, missing_docs)]
        #[doc(hidden)]
        #vis struct #name {}

        #[allow(non_camel_case_types)]
        impl #name {
            #deferred_checks
        }

        impl ::lesto::RouteInfo for #name {
            fn meta() -> ::lesto::RouteMeta {
                ::lesto::RouteMeta::new(::lesto::http::Method::#method_ident, #path)
                    .name(#name_str)
                    .summary(#summary)
                    #operation_id
                    #description
                    #status
                    #deprecated
                    #(#tags)*
                    #(#responses)*
                    #(#security)*
                    #public
            }
        }
    }
}

macro_rules! route_macro {
    ($($name:ident => $method:literal),* $(,)?) => {
        $(
            #[doc = concat!("Declare a `", $method, "` route: `#[lesto::", stringify!($name), "(\"/path\", status = 200, tag = \"x\", responses(404))]`.")]
            #[proc_macro_attribute]
            pub fn $name(attr: TokenStream, item: TokenStream) -> TokenStream {
                route($method, attr, item)
            }
        )*
    };
}

route_macro! {
    get => "GET",
    post => "POST",
    put => "PUT",
    patch => "PATCH",
    delete => "DELETE",
    head => "HEAD",
    options => "OPTIONS",
}

// ---- #[lesto::views] ------------------------------------------------------------------------

/// `Create(author, text?)`: a view name and the fields it keeps; `?` makes a field optional.
struct ViewSpec {
    name: Ident,
    fields: Vec<(Ident, bool)>,
}

impl Parse for ViewSpec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        let content;
        syn::parenthesized!(content in input);
        let mut fields = Vec::new();
        while !content.is_empty() {
            let field: Ident = content.parse()?;
            let optional = content.peek(Token![?]);
            if optional {
                content.parse::<Token![?]>()?;
            }
            fields.push((field, optional));
            if !content.is_empty() {
                content.parse::<Token![,]>()?;
            }
        }
        if fields.is_empty() {
            return Err(syn::Error::new(
                name.span(),
                "a view needs at least one field, e.g. `Create(author, text)`",
            ));
        }
        Ok(ViewSpec { name, fields })
    }
}

struct ViewsArgs(Vec<ViewSpec>);

impl Parse for ViewsArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let specs = Punctuated::<ViewSpec, Token![,]>::parse_terminated(input)?;
        if specs.is_empty() {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "expected at least one view, e.g. #[lesto::views(Create(author, text), Update(text?))]",
            ));
        }
        Ok(ViewsArgs(specs.into_iter().collect()))
    }
}

fn snake_case(ident: &Ident) -> String {
    let mut out = String::new();
    for (i, c) in ident.to_string().chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Derives a view keeps from its model: the std ones plus serde, schemars and garde. A view is
/// a request payload; derives of other crates (`sqlx::FromRow`, builders, ORM rows) describe
/// the model's storage, not its wire shape, and are dropped together with their field
/// attributes (`#[sqlx(..)]`).
const VIEW_DERIVES: &[&str] = &[
    "Debug",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Default",
    "Serialize",
    "Deserialize",
    "JsonSchema",
    "Validate",
];

/// Attribute namespaces a view keeps on the struct and on its fields.
const VIEW_ATTRS: &[&str] = &[
    "serde",
    "garde",
    "schemars",
    "allow",
    "cfg_attr",
    "doc",
    "deprecated",
];

fn is_view_attr(attr: &syn::Attribute) -> bool {
    VIEW_ATTRS.iter().any(|name| attr.path().is_ident(name))
}

/// Attributes of the model struct that the views inherit: the derives in [`VIEW_DERIVES`] and
/// the container attributes in [`VIEW_ATTRS`] (doc comments excepted: each view gets its own).
fn inherited_struct_attrs(attrs: &[syn::Attribute]) -> syn::Result<Vec<syn::Attribute>> {
    let mut out = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("derive") {
            let derives =
                attr.parse_args_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)?;
            let kept: Punctuated<syn::Path, Token![,]> = derives
                .into_iter()
                .filter(|p| {
                    p.segments
                        .last()
                        .is_some_and(|s| VIEW_DERIVES.contains(&s.ident.to_string().as_str()))
                })
                .collect();
            if !kept.is_empty() {
                out.push(syn::parse_quote! { #[derive(#kept)] });
            }
        } else if is_view_attr(attr) && !attr.path().is_ident("doc") {
            out.push(attr.clone());
        }
    }
    Ok(out)
}

/// A field's attributes as a view copies them: only the namespaces in [`VIEW_ATTRS`].
fn view_field_attrs(attrs: &[syn::Attribute]) -> Vec<syn::Attribute> {
    attrs.iter().filter(|a| is_view_attr(a)).cloned().collect()
}

/// Does `#[serde(..)]` mention `default` (bare or `default = "fn"`)?
fn serde_has_default(attr: &syn::Attribute) -> bool {
    let Meta::List(_) = &attr.meta else {
        return false;
    };
    attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .map(|metas| metas.iter().any(|m| m.path().is_ident("default")))
        .unwrap_or(false)
}

/// Rewrite a field's attributes for an optional (`field?`) copy: garde rules wrap in
/// `inner(..)` so they apply to the `Some` value, and serde treats the field as optional.
fn optional_field_attrs(attrs: &[syn::Attribute]) -> Vec<syn::Attribute> {
    let mut out = Vec::new();
    let mut has_serde_default = false;
    for attr in view_field_attrs(attrs) {
        if attr.path().is_ident("garde")
            && let Meta::List(list) = &attr.meta
        {
            let is_skip = matches!(
                attr.parse_args::<Meta>(),
                Ok(Meta::Path(p)) if p.is_ident("skip")
            );
            if is_skip {
                out.push(attr.clone());
            } else {
                let tokens = &list.tokens;
                out.push(syn::parse_quote! { #[garde(inner(#tokens))] });
            }
            continue;
        }
        if attr.path().is_ident("serde") && serde_has_default(&attr) {
            has_serde_default = true;
        }
        out.push(attr);
    }
    if !has_serde_default {
        out.push(syn::parse_quote! { #[serde(default, skip_serializing_if = "Option::is_none")] });
    }
    out
}

fn expand_views(args: ViewsArgs, item: syn::ItemStruct) -> syn::Result<TokenStream2> {
    let syn::Fields::Named(named) = &item.fields else {
        return Err(syn::Error::new_spanned(
            &item.ident,
            "#[lesto::views] needs a struct with named fields",
        ));
    };
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.generics,
            "#[lesto::views] does not support generic structs",
        ));
    }
    let model = &item.ident;
    let vis = &item.vis;
    let struct_attrs = inherited_struct_attrs(&item.attrs)?;

    let mut seen = std::collections::HashSet::new();
    let mut generated = Vec::new();
    for spec in args.0 {
        let view_ident = format_ident!("{}{}", model, spec.name);
        if !seen.insert(spec.name.to_string()) {
            return Err(syn::Error::new(
                spec.name.span(),
                format!("view `{}` is declared twice", spec.name),
            ));
        }
        let mut fields = Vec::new();
        let mut assignments = Vec::new();
        let mut seen_fields = std::collections::HashSet::new();
        for (field_name, optional) in &spec.fields {
            if !seen_fields.insert(field_name.to_string()) {
                return Err(syn::Error::new(
                    field_name.span(),
                    format!("field `{field_name}` listed twice in view `{}`", spec.name),
                ));
            }
            let Some(field) = named
                .named
                .iter()
                .find(|f| f.ident.as_ref() == Some(field_name))
            else {
                let available: Vec<String> = named
                    .named
                    .iter()
                    .filter_map(|f| f.ident.as_ref().map(|i| i.to_string()))
                    .collect();
                return Err(syn::Error::new(
                    field_name.span(),
                    format!(
                        "`{model}` has no field `{field_name}`; available fields: {}",
                        available.join(", ")
                    ),
                ));
            };
            let ty = &field.ty;
            let field_vis = &field.vis;
            if *optional {
                let attrs = optional_field_attrs(&field.attrs);
                fields.push(
                    quote! { #(#attrs)* #field_vis #field_name: ::core::option::Option<#ty> },
                );
                assignments.push(quote! {
                    if let ::core::option::Option::Some(value) = view.#field_name { self.#field_name = value; }
                });
            } else {
                let attrs = view_field_attrs(&field.attrs);
                fields.push(quote! { #(#attrs)* #field_vis #field_name: #ty });
                assignments.push(quote! { self.#field_name = view.#field_name; });
            }
        }
        let field_list = spec
            .fields
            .iter()
            .map(|(f, o)| {
                if *o {
                    format!("`{f}` (optional)")
                } else {
                    format!("`{f}`")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let doc = format!("`{}` view of [`{model}`]: {field_list}.", spec.name);
        let apply = format_ident!("apply_{}", snake_case(&spec.name));
        let apply_doc = format!(
            "Copy the fields of a [`{view_ident}`] into this `{model}`; optional fields are copied only when present."
        );
        generated.push(quote! {
            #[doc = #doc]
            #(#struct_attrs)*
            #vis struct #view_ident {
                #(#fields,)*
            }

            impl #model {
                #[doc = #apply_doc]
                #[allow(dead_code)]
                #vis fn #apply(&mut self, view: #view_ident) {
                    #(#assignments)*
                }
            }
        });
    }

    Ok(quote! {
        #item
        #(#generated)*
    })
}

/// Generate view structs from a model: `#[lesto::views(Create(author, text), Update(text?))]`
/// on `struct Note` emits `NoteCreate { author, text }` and `NoteUpdate { text: Option<String> }`,
/// copying each field with its serde, garde, schemars and doc attributes, plus
/// `Note::apply_create` / `Note::apply_update`.
///
/// Put the attribute **above** `#[derive(...)]`: the views inherit the derives and container
/// attributes (`#[serde(rename_all)]`, `#[garde(allow_unvalidated)]`, ...) written below it.
#[proc_macro_attribute]
pub fn views(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match syn::parse::<ViewsArgs>(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let item = match syn::parse::<syn::ItemStruct>(item) {
        Ok(s) => s,
        Err(e) => return e.to_compile_error().into(),
    };
    match expand_views(args, item) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ---- #[derive(Store)] (feature `db`) -----------------------------------------------------------

/// Newtype wrapper around `lesto::db::Store<M, P, DB>` (feature `db`): forwards `FromRequestParts` and
/// `OperationInput` to the inner store and derefs to it.
///
/// ```ignore
/// #[derive(lesto::db::Store)]
/// pub struct NoteStore<M, P>(lesto::db::Store<M, P, Sqlite>);
/// ```
///
/// With `#[store(read = "..", write = "..")]` the permission pair is declared once on the type
/// and `read` / `write` / `read_with` / `write_with` are generated without a requirement
/// argument, so a method cannot name the wrong permission:
///
/// ```ignore
/// #[derive(lesto::db::Store)]
/// #[store(read = "notes:read", write = "notes:write")]
/// pub struct NoteStore<M, P>(lesto::db::Store<M, P, Sqlite>);
/// ```
#[proc_macro_derive(Store, attributes(store))]
pub fn derive_store(item: TokenStream) -> TokenStream {
    let input = match syn::parse::<syn::DeriveInput>(item) {
        Ok(i) => i,
        Err(e) => return e.to_compile_error().into(),
    };
    expand_store(input)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

/// The permission pair declared by `#[store(read = "..", write = "..")]`.
#[derive(Default)]
struct StorePermissions {
    read: Option<syn::LitStr>,
    write: Option<syn::LitStr>,
}

impl StorePermissions {
    fn parse(attrs: &[syn::Attribute]) -> syn::Result<Self> {
        let mut found = Self::default();
        for attr in attrs.iter().filter(|a| a.path().is_ident("store")) {
            attr.parse_nested_meta(|meta| {
                let slot = if meta.path.is_ident("read") {
                    &mut found.read
                } else if meta.path.is_ident("write") {
                    &mut found.write
                } else {
                    return Err(meta.error(
                        "unknown `#[store(..)]` option: expected `read` or `write`, each a permission string",
                    ));
                };
                if slot.is_some() {
                    return Err(meta.error("this permission is already declared"));
                }
                *slot = Some(meta.value()?.parse::<syn::LitStr>()?);
                Ok(())
            })?;
        }
        if let Some(attr) = attrs.iter().find(|a| a.path().is_ident("store"))
            && found.read.is_none()
            && found.write.is_none()
        {
            return Err(syn::Error::new_spanned(
                attr,
                "`#[store(..)]` declares nothing: give it `read = \"..\"`, `write = \"..\"`, or both",
            ));
        }
        Ok(found)
    }

    fn declared(&self) -> bool {
        self.read.is_some() || self.write.is_some()
    }
}

/// The `M`, `P` and `DB` of the inner `Store<M, P, DB>`, as written.
fn store_arguments(inner: &Type) -> syn::Result<(Type, Type, Type)> {
    const SHAPE: &str = "`#[store(..)]` needs the inner type spelled as `lesto::db::Store<M, P, Db>` so the mode, the principal and the database can be named in the generated methods";
    let Type::Path(path) = inner else {
        return Err(syn::Error::new_spanned(inner, SHAPE));
    };
    let segment = path
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(inner, SHAPE))?;
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new_spanned(inner, SHAPE));
    };
    let types: Vec<Type> = args
        .args
        .iter()
        .filter_map(|a| match a {
            syn::GenericArgument::Type(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    match <[Type; 3]>::try_from(types) {
        Ok([mode, principal, db]) => Ok((mode, principal, db)),
        Err(_) => Err(syn::Error::new_spanned(inner, SHAPE)),
    }
}

fn expand_store(input: syn::DeriveInput) -> syn::Result<TokenStream2> {
    const SHAPE: &str = "#[derive(Store)] expects a tuple struct with exactly one field holding the inner store, e.g. `struct NoteStore<M, P>(lesto::db::Store<M, P, Sqlite>);`";
    let inner: Type = match &input.data {
        syn::Data::Struct(syn::DataStruct {
            fields: syn::Fields::Unnamed(fields),
            ..
        }) if fields.unnamed.len() == 1 => fields.unnamed[0].ty.clone(),
        _ => return Err(syn::Error::new_spanned(&input.ident, SHAPE)),
    };
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let predicates = where_clause.map(|w| &w.predicates);

    // `impl<__S, ..generics>`: splice the extra state parameter in front of the user's.
    let mut extract_generics = input.generics.clone();
    extract_generics
        .params
        .insert(0, syn::parse_quote!(__S: Send + Sync));
    let (extract_impl_generics, _, _) = extract_generics.split_for_impl();

    let permissions = StorePermissions::parse(&input.attrs)?;
    let guarded = if permissions.declared() {
        let (mode, principal, db) = store_arguments(&inner)?;
        let connection = quote! {
            <#db as ::lesto::db::sqlx::Database>::Connection
        };
        let read = permissions.read.map(|permission| {
            let doc = format!(
                "Run `f` in a read-only transaction, requiring `{}`.",
                permission.value()
            );
            let doc_with = format!(
                "[`read`](Self::read) at `isolation`, requiring `{}`.",
                permission.value()
            );
            quote! {
                impl #impl_generics #name #ty_generics
                where
                    #mode: ::lesto::db::Mode,
                    #principal: ::lesto::db::Authenticated,
                    #db: ::lesto::db::Dialect,
                    #predicates
                {
                    #[doc = #doc]
                    pub async fn read<__T, __E, __F>(
                        &self,
                        f: __F,
                    ) -> ::core::result::Result<__T, ::lesto::db::Error>
                    where
                        __E: ::core::convert::Into<::lesto::db::Error>,
                        __F: for<'__c> ::core::ops::AsyncFnOnce(&'__c mut #connection)
                            -> ::core::result::Result<__T, __E>,
                    {
                        self.0.read(#permission, f).await
                    }

                    #[doc = #doc_with]
                    pub async fn read_with<__T, __E, __F>(
                        &self,
                        isolation: ::lesto::db::Isolation,
                        f: __F,
                    ) -> ::core::result::Result<__T, ::lesto::db::Error>
                    where
                        __E: ::core::convert::Into<::lesto::db::Error>,
                        __F: for<'__c> ::core::ops::AsyncFn(&'__c mut #connection)
                            -> ::core::result::Result<__T, __E>,
                    {
                        self.0.read_with(#permission, isolation, f).await
                    }
                }
            }
        });
        let write = permissions.write.map(|permission| {
            let doc = format!(
                "Run `f` in a read-write transaction, requiring `{}`.",
                permission.value()
            );
            let doc_with = format!(
                "[`write`](Self::write) at `isolation`, requiring `{}`.",
                permission.value()
            );
            quote! {
                impl #impl_generics #name #ty_generics
                where
                    #mode: ::lesto::db::Writable,
                    #principal: ::lesto::db::Authenticated,
                    #db: ::lesto::db::Dialect,
                    #predicates
                {
                    #[doc = #doc]
                    pub async fn write<__T, __E, __F>(
                        &self,
                        f: __F,
                    ) -> ::core::result::Result<__T, ::lesto::db::Error>
                    where
                        __E: ::core::convert::Into<::lesto::db::Error>,
                        __F: for<'__c> ::core::ops::AsyncFnOnce(&'__c mut #connection)
                            -> ::core::result::Result<__T, __E>,
                    {
                        self.0.write(#permission, f).await
                    }

                    #[doc = #doc_with]
                    pub async fn write_with<__T, __E, __F>(
                        &self,
                        isolation: ::lesto::db::Isolation,
                        f: __F,
                    ) -> ::core::result::Result<__T, ::lesto::db::Error>
                    where
                        __E: ::core::convert::Into<::lesto::db::Error>,
                        __F: for<'__c> ::core::ops::AsyncFn(&'__c mut #connection)
                            -> ::core::result::Result<__T, __E>,
                    {
                        self.0.write_with(#permission, isolation, f).await
                    }
                }
            }
        });
        quote! { #read #write }
    } else {
        TokenStream2::new()
    };

    Ok(quote! {
        #guarded

        impl #extract_impl_generics ::lesto::__private::FromRequestParts<__S> for #name #ty_generics
        where
            #inner: ::lesto::__private::FromRequestParts<__S>,
            #predicates
        {
            type Rejection = <#inner as ::lesto::__private::FromRequestParts<__S>>::Rejection;

            async fn from_request_parts(
                parts: &mut ::lesto::__private::Parts,
                state: &__S,
            ) -> ::core::result::Result<Self, Self::Rejection> {
                let inner = <#inner as ::lesto::__private::FromRequestParts<__S>>::from_request_parts(parts, state).await?;
                ::core::result::Result::Ok(#name(inner))
            }
        }

        impl #impl_generics ::lesto::__private::OperationInput for #name #ty_generics
        where
            #inner: ::lesto::__private::OperationInput,
            #predicates
        {
            fn describe(builder: &mut ::lesto::__private::OperationBuilder<'_>) {
                <#inner as ::lesto::__private::OperationInput>::describe(builder);
            }
        }

        impl #impl_generics ::core::ops::Deref for #name #ty_generics #where_clause {
            type Target = #inner;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl #impl_generics ::core::ops::DerefMut for #name #ty_generics #where_clause {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(attr: &str, item: &str) -> String {
        let args: ViewsArgs = syn::parse_str(attr).unwrap();
        let item: syn::ItemStruct = syn::parse_str(item).unwrap();
        expand_views(args, item).unwrap().to_string()
    }

    #[test]
    fn views_drop_derives_and_attributes_of_other_crates() {
        let out = expand(
            "Create(text)",
            r#"
            #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Validate, sqlx::FromRow)]
            #[serde(rename_all = "camelCase")]
            struct Note {
                #[garde(skip)]
                #[sqlx(rename = "note_id")]
                id: i64,
                /// The text.
                #[garde(length(min = 1))]
                #[sqlx(rename = "body")]
                text: String,
            }
            "#,
        );
        let view = out.split("struct NoteCreate").nth(1).unwrap();
        let view_head = out.split("struct NoteCreate").next().unwrap();
        let view_attrs = view_head.rsplit("struct Note").next().unwrap();
        assert!(!view_attrs.contains("FromRow"), "{view_attrs}");
        assert!(!view.contains("sqlx"), "{view}");
        assert!(view_attrs.contains("Deserialize"), "{view_attrs}");
        assert!(view_attrs.contains("rename_all"), "{view_attrs}");
        assert!(view.contains("garde"), "{view}");
        assert!(view.contains("The text."), "{view}");
    }

    #[test]
    fn optional_fields_detect_serde_default_by_meaning_not_by_substring() {
        let out = expand(
            "Update(text?)",
            r#"
            #[derive(Deserialize)]
            struct Note {
                #[serde(rename = "default_text")]
                text: String,
            }
            "#,
        );
        let view = out.split("struct NoteUpdate").nth(1).unwrap();
        assert!(
            view.contains("default ,") || view.contains("default,"),
            "{view}"
        );
        assert!(view.contains("skip_serializing_if"), "{view}");

        let out = expand(
            "Update(text?)",
            r#"
            #[derive(Deserialize)]
            struct Note {
                #[serde(default = "my_default")]
                text: String,
            }
            "#,
        );
        let view = out.split("struct NoteUpdate").nth(1).unwrap();
        assert!(
            !view.contains("skip_serializing_if"),
            "an explicit serde default is kept as is: {view}"
        );
    }

    #[test]
    fn optional_fields_wrap_garde_rules_but_not_skip() {
        let out = expand(
            "Update(a?, b?)",
            r#"
            struct M {
                #[garde(skip)]
                a: u8,
                #[garde(range(min = 1))]
                b: u8,
            }
            "#,
        );
        let view = out.split("struct MUpdate").nth(1).unwrap();
        assert!(view.contains("garde (skip)"), "{view}");
        assert!(view.contains("garde (inner (range (min = 1)))"), "{view}");
    }
}

//! Describe handler inputs and outputs as OpenAPI operations.
//!
//! [`OperationInput`] is implemented by extractors, [`OperationOutput`] by response types.
//! [`OperationHandler`] is implemented for every async fn and exposes the tuple of argument
//! types (`I`) and the return type (`O`) so that [`crate::RouteSet`] can document a handler
//! with no annotations beyond the route itself.

use std::future::Future;

use http::StatusCode;
use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::Value;

use crate::error::Problem;
use crate::openapi::{
    self, MediaType, Operation, Parameter, ParameterIn, RequestBody, Response, SecurityRequirement,
    SecurityScheme,
};

/// Mutable view over an [`Operation`] being built, plus the shared schema generator.
pub struct OperationBuilder<'a> {
    /// The operation being described.
    pub operation: &'a mut Operation,
    /// Shared generator: schemas land in `components.schemas`.
    pub generator: &'a mut SchemaGenerator,
    /// OpenAPI path template of the route, e.g. `/users/{id}`.
    pub path: &'a str,
    /// HTTP method of the route.
    pub method: &'a http::Method,
    /// `components.securitySchemes`, shared by every operation.
    pub security_schemes: &'a mut IndexMap<String, SecurityScheme>,
}

impl OperationBuilder<'_> {
    /// Require security scheme `name` (registering `scheme` in components if new) with `scopes`.
    ///
    /// Each call adds an *alternative* requirement: any one of them satisfies the operation.
    pub fn security(&mut self, name: &str, scheme: SecurityScheme, scopes: &[&str]) {
        self.security_schemes
            .entry(name.to_string())
            .or_insert(scheme);
        self.security_requirement(name, scopes);
    }

    /// Require an already registered scheme (see [`crate::App::security_scheme`]).
    pub fn security_requirement(&mut self, name: &str, scopes: &[&str]) {
        let mut requirement = SecurityRequirement::new();
        requirement.insert(
            name.to_string(),
            scopes.iter().map(|s| s.to_string()).collect(),
        );
        let list = self.operation.security.get_or_insert_with(Vec::new);
        if !list.contains(&requirement) {
            list.push(requirement);
        }
        if !self.operation.responses.contains_key("401") {
            self.error_response(401, "Unauthorized");
        }
    }

    /// Document an error response: an RFC 9457 [`Problem`].
    pub fn error_response(&mut self, status: impl ResponseKey, description: &str) {
        self.response::<Problem>(status, description, crate::error::PROBLEM_JSON);
    }

    /// Document the `422` every validating extractor can produce.
    pub fn validation_error_response(&mut self) {
        self.response::<Problem>(422, "Validation Error", crate::error::PROBLEM_JSON);
    }

    /// Schema (usually a `$ref`) for `T`, registering it in components when named.
    ///
    /// Boolean schemas (`true` for `serde_json::Value`) are turned into objects, which is what
    /// Swagger UI and most generators expect.
    pub fn schema_for<T: JsonSchema + ?Sized>(&mut self) -> Schema {
        let mut schema = self.generator.subschema_for::<T>();
        schema.ensure_object();
        schema
    }

    /// The full (non-`$ref`) schema for `T`, used to read properties out of it.
    fn resolved_schema_for<T: JsonSchema + ?Sized>(&mut self) -> Schema {
        let schema = T::json_schema(self.generator);
        if let Some(Value::String(reference)) = schema.get("$ref") {
            let name = reference.rsplit('/').next().unwrap_or_default();
            if let Some(def) = self.generator.definitions().get(name) {
                return Schema::try_from(def.clone()).unwrap_or(schema);
            }
        }
        schema
    }

    /// Document a request body of type `T` with the given media type.
    pub fn request_body<T: JsonSchema + ?Sized>(&mut self, content_type: &str, required: bool) {
        let schema = self.schema_for::<T>();
        let body = self
            .operation
            .request_body
            .get_or_insert_with(RequestBody::default);
        body.required |= required;
        body.content.insert(
            content_type.to_string(),
            MediaType {
                schema: Some(schema),
            },
        );
    }

    /// Document a response of type `T` under `status`.
    pub fn response<T: JsonSchema + ?Sized>(
        &mut self,
        status: impl ResponseKey,
        description: &str,
        content_type: &str,
    ) {
        let schema = self.schema_for::<T>();
        let response = self.response_entry(status, description);
        response.content.insert(
            content_type.to_string(),
            MediaType {
                schema: Some(schema),
            },
        );
    }

    /// Document a response with no body under `status`.
    pub fn empty_response(&mut self, status: impl ResponseKey, description: &str) {
        self.response_entry(status, description);
    }

    fn response_entry(&mut self, status: impl ResponseKey, description: &str) -> &mut Response {
        let key = status.key();
        if !self.operation.responses.contains_key(&key) {
            self.operation.responses.insert(
                key.clone(),
                Response {
                    description: description.to_string(),
                    content: IndexMap::new(),
                },
            );
        }
        self.operation
            .responses
            .get_mut(&key)
            .expect("just inserted")
    }

    /// Add one parameter per property of `T`'s object schema.
    pub fn parameters_from_object<T: JsonSchema + ?Sized>(&mut self, location: ParameterIn) {
        let schema = self.resolved_schema_for::<T>();
        let required: Vec<String> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let Some(Value::Object(props)) = schema.get("properties") else {
            return;
        };
        for (name, prop) in props {
            let mut prop_schema = Schema::try_from(prop.clone()).unwrap_or_default();
            let description = prop_schema
                .remove("description")
                .and_then(|d| d.as_str().map(String::from));
            let deprecated = prop_schema
                .remove("deprecated")
                .and_then(|d| d.as_bool())
                .unwrap_or(false);
            self.operation.parameters.push(Parameter {
                name: name.clone(),
                location,
                description,
                required: location == ParameterIn::Path || required.contains(name),
                deprecated,
                schema: Some(prop_schema),
            });
        }
    }

    /// Add the `{name}` path parameters of this route, typed from `T`.
    ///
    /// `T` may be a struct (matched by field name), a tuple (matched positionally),
    /// or a single scalar (the only placeholder).
    pub fn path_parameters<T: JsonSchema + ?Sized>(&mut self) {
        let names = openapi::path_param_names(self.path);
        if names.is_empty() {
            return;
        }
        let schema = self.resolved_schema_for::<T>();
        let properties = schema.get("properties").and_then(Value::as_object).cloned();
        let items = schema.get("prefixItems").and_then(Value::as_array).cloned();
        for (i, name) in names.iter().enumerate() {
            let prop = match (&properties, &items) {
                (Some(props), _) => props.get(name).cloned(),
                (None, Some(items)) => items.get(i).cloned(),
                (None, None) if names.len() == 1 => Some(schema.clone().to_value()),
                _ => None,
            };
            let mut prop_schema =
                prop.and_then(|p| Schema::try_from(p).ok())
                    .unwrap_or_else(|| {
                        Schema::try_from(serde_json::json!({"type": "string"})).unwrap()
                    });
            let description = prop_schema
                .remove("description")
                .and_then(|d| d.as_str().map(String::from));
            self.operation.parameters.push(Parameter {
                name: name.clone(),
                location: ParameterIn::Path,
                description,
                required: true,
                deprecated: false,
                schema: Some(prop_schema),
            });
        }
    }
}

/// Key of the `responses` map: a status code or a range such as `"default"`.
pub trait ResponseKey {
    /// The key as it appears in the document.
    fn key(self) -> String;
}

impl ResponseKey for u16 {
    fn key(self) -> String {
        self.to_string()
    }
}

impl ResponseKey for StatusCode {
    fn key(self) -> String {
        self.as_u16().to_string()
    }
}

impl ResponseKey for &str {
    fn key(self) -> String {
        self.to_string()
    }
}

/// Standard reason phrase for a status, e.g. `"Not Found"`.
pub fn reason_phrase(status: u16) -> String {
    StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("Response")
        .to_string()
}

/// An extractor that can describe itself in an OpenAPI operation.
#[diagnostic::on_unimplemented(
    message = "lesto cannot document `{Self}` as a handler argument",
    label = "`{Self}` does not implement `lesto::OperationInput`",
    note = "request data goes through `lesto::Json<T>`, `lesto::Query<T>` or `lesto::Path<T>`, with `T: serde::Deserialize + schemars::JsonSchema` (+ `garde::Validate` for Json/Query)",
    note = "for a custom extractor, add `impl lesto::OperationInput for {Self} {{}}` (an empty impl documents nothing) or delegate to the extractor it wraps, e.g. `Bearer::<BearerAuth>::describe(builder)`"
)]
pub trait OperationInput {
    /// Add what this argument contributes (parameters, body, security) to the operation.
    fn describe(builder: &mut OperationBuilder<'_>) {
        let _ = builder;
    }
}

/// A response type that can describe itself in an OpenAPI operation.
///
/// `status` is the success status of the route (200 unless overridden).
#[diagnostic::on_unimplemented(
    message = "lesto cannot document `{Self}` as a handler response",
    label = "`{Self}` does not implement `lesto::OperationOutput`",
    note = "return `lesto::Json<T>` with `T: serde::Serialize + schemars::JsonSchema`, or `String`, `&'static str`, `()`, `Html<T>`, or `Result<T, lesto::HttpError>` of those",
    note = "for a custom response type, add `impl lesto::OperationOutput for {Self} {{}}` (an empty impl documents nothing)"
)]
pub trait OperationOutput {
    /// Add the responses this type produces to the operation; `status` is the success status.
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        let _ = (builder, status);
    }
}

// ---- inputs that carry no documentation --------------------------------------------------

impl<S> OperationInput for axum::extract::State<S> {}
impl<T> OperationInput for axum::extract::Extension<T> {}
impl OperationInput for http::HeaderMap {}
impl OperationInput for http::Method {}
impl OperationInput for http::Uri {}
impl OperationInput for http::Version {}
impl OperationInput for http::request::Parts {}
impl OperationInput for axum::extract::Request {}
impl OperationInput for axum::body::Body {}
impl OperationInput for axum::body::Bytes {}
impl OperationInput for String {}
impl OperationInput for axum::extract::RawQuery {}
impl OperationInput for axum::extract::OriginalUri {}
impl OperationInput for axum::extract::MatchedPath {}
impl OperationInput for axum::extract::NestedPath {}
impl<T: OperationInput> OperationInput for Option<T> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        T::describe(builder);
    }
}
impl<T: OperationInput, E> OperationInput for Result<T, E> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        T::describe(builder);
    }
}

/// axum's own `Json` (no validation) still documents its body.
impl<T: JsonSchema> OperationInput for axum::Json<T> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.request_body::<T>("application/json", true);
    }
}

/// axum's own `Query` (no validation) still documents its parameters.
impl<T: JsonSchema> OperationInput for axum::extract::Query<T> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.parameters_from_object::<T>(ParameterIn::Query);
    }
}

/// axum's own `Path` still documents its parameters.
impl<T: JsonSchema> OperationInput for axum::extract::Path<T> {
    fn describe(builder: &mut OperationBuilder<'_>) {
        builder.path_parameters::<T>();
    }
}

macro_rules! impl_input_tuple {
    ($($T:ident),*) => {
        impl<$($T: OperationInput),*> OperationInput for ($($T,)*) {
            #[allow(unused_variables)]
            fn describe(builder: &mut OperationBuilder<'_>) {
                $( $T::describe(builder); )*
            }
        }
    };
}

impl_input_tuple!();
impl_input_tuple!(T1);
impl_input_tuple!(T1, T2);
impl_input_tuple!(T1, T2, T3);
impl_input_tuple!(T1, T2, T3, T4);
impl_input_tuple!(T1, T2, T3, T4, T5);
impl_input_tuple!(T1, T2, T3, T4, T5, T6);
impl_input_tuple!(T1, T2, T3, T4, T5, T6, T7);
impl_input_tuple!(T1, T2, T3, T4, T5, T6, T7, T8);
impl_input_tuple!(T1, T2, T3, T4, T5, T6, T7, T8, T9);
impl_input_tuple!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);
impl_input_tuple!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11);
impl_input_tuple!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12);
impl_input_tuple!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13);
impl_input_tuple!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14);
impl_input_tuple!(
    T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15
);
impl_input_tuple!(
    T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15, T16
);

// ---- outputs -------------------------------------------------------------------------------

impl OperationOutput for () {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        builder.empty_response(status, &reason_phrase(status));
    }
}

impl OperationOutput for String {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        builder.response::<String>(status, &reason_phrase(status), "text/plain; charset=utf-8");
    }
}

impl OperationOutput for &'static str {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        builder.response::<String>(status, &reason_phrase(status), "text/plain; charset=utf-8");
    }
}

impl<T: JsonSchema> OperationOutput for axum::Json<T> {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        builder.response::<T>(status, &reason_phrase(status), "application/json");
    }
}

impl<T: JsonSchema> OperationOutput for axum::response::Html<T> {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        builder.response::<String>(status, &reason_phrase(status), "text/html; charset=utf-8");
    }
}

impl OperationOutput for StatusCode {}
impl OperationOutput for axum::response::Response {}
impl OperationOutput for axum::response::Redirect {}
impl OperationOutput for std::convert::Infallible {}

impl<T: OperationOutput, E: OperationOutput> OperationOutput for Result<T, E> {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        T::describe(builder, status);
        E::describe(builder, status);
    }
}

impl<T: OperationOutput> OperationOutput for Option<T> {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        T::describe(builder, status);
    }
}

impl<T: OperationOutput> OperationOutput for Box<T> {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        T::describe(builder, status);
    }
}

impl<T: OperationOutput> OperationOutput for (StatusCode, T) {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        T::describe(builder, status);
    }
}

impl<T: OperationOutput> OperationOutput for (http::HeaderMap, T) {
    fn describe(builder: &mut OperationBuilder<'_>, status: u16) {
        T::describe(builder, status);
    }
}

/// Exposes a handler's argument tuple `I` and return type `O` at the type level.
///
/// Implemented for every `async fn` with up to 16 arguments, mirroring axum's `Handler`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a lesto handler",
    label = "expected an `async fn` with at most 16 arguments",
    note = "handlers are `async fn`s whose arguments are extractors (`Json<T>`, `Query<T>`, `Path<T>`, `State<S>`, ...) and whose return type is a response (`Json<T>`, `String`, `()`, `Result<T, HttpError>`, ...)",
    note = "an extractor that consumes the body (`Json<T>`, `String`, `Bytes`, `Request`) must be the last argument"
)]
pub trait OperationHandler<I, O> {}

macro_rules! impl_operation_handler {
    ($($T:ident),*) => {
        impl<F, Fut, O, $($T,)*> OperationHandler<($($T,)*), O> for F
        where
            F: FnOnce($($T,)*) -> Fut,
            Fut: Future<Output = O>,
        {
        }
    };
}

impl_operation_handler!();
impl_operation_handler!(T1);
impl_operation_handler!(T1, T2);
impl_operation_handler!(T1, T2, T3);
impl_operation_handler!(T1, T2, T3, T4);
impl_operation_handler!(T1, T2, T3, T4, T5);
impl_operation_handler!(T1, T2, T3, T4, T5, T6);
impl_operation_handler!(T1, T2, T3, T4, T5, T6, T7);
impl_operation_handler!(T1, T2, T3, T4, T5, T6, T7, T8);
impl_operation_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9);
impl_operation_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);
impl_operation_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11);
impl_operation_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12);
impl_operation_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13);
impl_operation_handler!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14);
impl_operation_handler!(
    T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15
);
impl_operation_handler!(
    T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15, T16
);

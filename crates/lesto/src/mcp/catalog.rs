//! Operations marked `mcp = ".."` turned into MCP tools, resources and prompts, from the OpenAPI
//! document lesto already builds. A tool gets one input schema out of the path and query
//! parameters and the JSON body, the success response as the output schema and hints from the
//! HTTP method; a resource gets a URI (a template when the path has parameters); a prompt gets
//! its parameters as arguments.

use std::collections::{BTreeSet, HashMap, HashSet};

use http::Method;
use serde_json::{Map, Value, json};

use crate::openapi::{OpenApi, Operation, Parameter, ParameterIn};
use crate::route::{McpExpose, PendingOperation};

const COMPONENTS_PREFIX: &str = "#/components/schemas/";
const DEFS_PREFIX: &str = "#/$defs/";

/// Where the JSON body comes from in a tool's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyArgs {
    /// The operation has no JSON body.
    None,
    /// The body's properties sit next to the parameters: the body is every argument that is
    /// not a parameter.
    Flat,
    /// The body is the `body` argument: it is not an object, or a property name collides with
    /// a parameter.
    Wrapped { required: bool },
}

/// A path parameter of the template: `{id}`, or `{*rest}` which may span segments.
#[derive(Debug, Clone)]
pub(crate) struct PathParam {
    pub name: String,
    pub wildcard: bool,
}

/// The route an MCP primitive calls, and how its arguments map onto a request.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    /// The tool, resource or prompt name (for `McpCall` and the logs).
    pub name: String,
    pub method: Method,
    /// Final path template, axum syntax.
    pub path: String,
    pub path_params: Vec<PathParam>,
    pub query_params: Vec<String>,
    pub body: BodyArgs,
}

#[derive(Debug, Clone)]
pub(crate) struct Tool {
    pub target: Target,
    /// The tool as `tools/list` shows it, without `outputSchema`.
    pub definition: Map<String, Value>,
    /// The success response's schema, and whether it is an object schema (the only kind legacy
    /// revisions accept).
    pub output_schema: Option<(Value, bool)>,
}

impl Tool {
    /// The definition for `tools/list`: legacy revisions only accept an object output schema.
    pub fn definition(&self, modern: bool) -> Value {
        let mut definition = self.definition.clone();
        if let Some((schema, is_object)) = &self.output_schema
            && (modern || *is_object)
        {
            definition.insert("outputSchema".into(), schema.clone());
        }
        Value::Object(definition)
    }

    /// Whether `structuredContent` goes with a successful result.
    pub fn structured(&self, modern: bool) -> bool {
        matches!(&self.output_schema, Some((_, is_object)) if modern || *is_object)
    }
}

/// A `GET` route read as an MCP resource: a fixed URI, or a URI template when its path has
/// parameters.
#[derive(Debug, Clone)]
pub(crate) struct Resource {
    pub target: Target,
    /// The entry of `resources/list` (`uri`) or `resources/templates/list` (`uriTemplate`).
    pub definition: Value,
    pub template: bool,
}

impl Resource {
    /// Whether `path` (a request path, still percent-encoded) is one this route serves: literal
    /// segments equal, a parameter any non-empty segment, a wildcard the non-empty rest.
    pub fn matches(&self, path: &str) -> bool {
        let mut wanted = self.target.path.split('/');
        let mut given = path.split('/');
        loop {
            match (wanted.next(), given.next()) {
                (None, None) => return true,
                (Some(w), Some(g)) if w.starts_with("{*") && w.ends_with('}') => {
                    return !g.is_empty() || given.any(|g| !g.is_empty());
                }
                (Some(w), Some(g)) if w.starts_with('{') && w.ends_with('}') => {
                    if g.is_empty() {
                        return false;
                    }
                }
                (Some(w), Some(g)) if w == g => {}
                _ => return false,
            }
        }
    }
}

/// A `GET` route returning `lesto::mcp::Prompt`: its parameters are the prompt's arguments.
#[derive(Debug, Clone)]
pub(crate) struct Prompt {
    pub target: Target,
    /// The entry of `prompts/list`.
    pub definition: Value,
}

#[derive(Debug, Default)]
pub(crate) struct Catalog {
    pub tools: Vec<Tool>,
    tool_index: HashMap<String, usize>,
    pub resources: Vec<Resource>,
    pub prompts: Vec<Prompt>,
    prompt_index: HashMap<String, usize>,
}

/// The three kinds of primitive, to name them one namespace at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Tool,
    Resource,
    Prompt,
}

impl Kind {
    fn of(expose: &McpExpose) -> Kind {
        match expose {
            McpExpose::Tool { .. } => Kind::Tool,
            McpExpose::Resource { .. } => Kind::Resource,
            McpExpose::Prompt { .. } => Kind::Prompt,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Kind::Tool => "tool",
            Kind::Resource => "resource",
            Kind::Prompt => "prompt",
        }
    }
}

impl Catalog {
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tool_index.get(name).map(|&i| &self.tools[i])
    }

    pub fn prompt(&self, name: &str) -> Option<&Prompt> {
        self.prompt_index.get(name).map(|&i| &self.prompts[i])
    }

    /// The resource serving `path`: a fixed URI first, then the templates in registration
    /// order, as a router would try them.
    pub fn resource(&self, path: &str) -> Option<&Resource> {
        self.resources
            .iter()
            .find(|r| !r.template && r.target.path == path)
            .or_else(|| {
                self.resources
                    .iter()
                    .find(|r| r.template && r.matches(path))
            })
    }

    /// The tools, resources and prompts of `operations`, in registration order, documented from
    /// `spec` (which must have been built from the same operations). Resource URIs start with
    /// `resource_base`.
    ///
    /// Panics, like axum on conflicting routes, when two operations of one kind ask for the same
    /// explicit name, a tool's body is not JSON, or a resource needs a query parameter: those
    /// are mistakes in the application, found at startup.
    pub fn build(
        operations: &[(String, PendingOperation)],
        spec: &OpenApi,
        resource_base: &str,
    ) -> Catalog {
        let exposed: Vec<(&String, &PendingOperation, &McpExpose)> = operations
            .iter()
            .filter_map(|(path, pending)| {
                pending
                    .meta
                    .mcp
                    .as_ref()
                    .map(|expose| (path, pending, expose))
            })
            .collect();
        let names = names(&exposed);

        let schemas = spec
            .components
            .as_ref()
            .map(|c| {
                c.schemas
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone().to_value()))
                    .collect()
            })
            .unwrap_or_default();
        let mut catalog = Catalog::default();
        for ((path, pending, expose), name) in exposed.into_iter().zip(names) {
            let meta = &pending.meta;
            let operation = spec
                .paths
                .get(path)
                .and_then(|item| item.operation(&meta.method))
                .unwrap_or_else(|| {
                    panic!(
                        "{} {path} is missing from the OpenAPI document",
                        meta.method
                    )
                });
            match Kind::of(expose) {
                Kind::Tool => {
                    let tool = tool(name, path, &meta.method, operation, &schemas);
                    catalog
                        .tool_index
                        .insert(tool.target.name.clone(), catalog.tools.len());
                    catalog.tools.push(tool);
                }
                Kind::Resource => {
                    let resource = resource(name, path, operation, resource_base);
                    catalog.resources.push(resource);
                }
                Kind::Prompt => {
                    let prompt = prompt(name, path, operation);
                    catalog
                        .prompt_index
                        .insert(prompt.target.name.clone(), catalog.prompts.len());
                    catalog.prompts.push(prompt);
                }
            }
        }
        catalog
    }
}

/// The name of each exposed operation, one namespace per kind: the explicit name, else the
/// handler's name, else (when two handlers of a kind share a name, or an explicit name takes
/// it) the `operationId`, which is unique by construction.
fn names(exposed: &[(&String, &PendingOperation, &McpExpose)]) -> Vec<String> {
    // Explicit names first: they must be unique, and they win over a default name.
    let mut explicit: HashMap<(Kind, &str), String> = HashMap::new();
    for (path, pending, expose) in exposed {
        if let Some(name) = expose.name() {
            let kind = Kind::of(expose);
            let route = format!("{} {path}", pending.meta.method);
            if let Some(other) = explicit.insert((kind, name), route.clone()) {
                let kind = kind.word();
                panic!(
                    "two routes are exposed as the MCP {kind} `{name}`: {other} and {route}; give one of them another `mcp({kind}, name = \"..\")`"
                );
            }
        }
    }
    let mut default_counts: HashMap<(Kind, &str), usize> = HashMap::new();
    for (_, pending, expose) in exposed {
        if expose.name().is_none()
            && let Some(fn_name) = &pending.meta.name
        {
            *default_counts
                .entry((Kind::of(expose), fn_name.as_str()))
                .or_default() += 1;
        }
    }

    let mut taken: HashSet<(Kind, String)> = HashSet::new();
    exposed
        .iter()
        .map(|(path, pending, expose)| {
            let kind = Kind::of(expose);
            let meta = &pending.meta;
            let name = match (expose.name(), &meta.name) {
                (Some(name), _) => name.to_string(),
                (None, Some(fn_name))
                    if default_counts.get(&(kind, fn_name.as_str())) == Some(&1)
                        && !explicit.contains_key(&(kind, fn_name.as_str())) =>
                {
                    fn_name.clone()
                }
                (None, _) => meta.default_operation_id(path),
            };
            if !taken.insert((kind, name.clone())) {
                let kind = kind.word();
                panic!(
                    "two routes are exposed as the MCP {kind} `{name}`; give one of them `mcp({kind}, name = \"..\")`"
                );
            }
            name
        })
        .collect()
}

/// The path and query parameters of `operation`, with the argument name (`rest` for
/// `{*rest}`) and whether the parameter is in the path and a wildcard.
fn parameters(operation: &Operation) -> impl Iterator<Item = (&Parameter, String, bool)> {
    operation
        .parameters
        .iter()
        .filter(|parameter| matches!(parameter.location, ParameterIn::Path | ParameterIn::Query))
        .map(|parameter| {
            let wildcard = parameter.name.starts_with('*');
            let arg = parameter.name.trim_start_matches('*').to_string();
            (parameter, arg, wildcard)
        })
}

/// The target of a route with no body: its parameters split into path and query.
fn target(name: String, method: &Method, path: &str, operation: &Operation) -> Target {
    let mut path_params = Vec::new();
    let mut query_params = Vec::new();
    for (parameter, arg, wildcard) in parameters(operation) {
        match parameter.location {
            ParameterIn::Path => path_params.push(PathParam {
                name: arg,
                wildcard,
            }),
            _ => query_params.push(arg),
        }
    }
    Target {
        name,
        method: method.clone(),
        path: path.to_string(),
        path_params,
        query_params,
        body: BodyArgs::None,
    }
}

/// A parameter's description: the parameter's own, else its schema's.
fn parameter_description(parameter: &Parameter) -> Option<String> {
    parameter.description.clone().or_else(|| {
        parameter.schema.as_ref().and_then(|s| {
            s.clone()
                .to_value()
                .get("description")?
                .as_str()
                .map(String::from)
        })
    })
}

/// The summary and description of the doc comment, as MCP's `title` and `description`.
fn describe(operation: &Operation, definition: &mut Map<String, Value>) {
    if let Some(summary) = &operation.summary {
        definition.insert("title".into(), summary.clone().into());
    }
    let description = match (&operation.summary, &operation.description) {
        (Some(summary), Some(description)) => Some(format!("{summary}\n\n{description}")),
        (Some(text), None) | (None, Some(text)) => Some(text.clone()),
        (None, None) => None,
    };
    if let Some(description) = description {
        definition.insert("description".into(), description.into());
    }
}

fn resource(name: String, path: &str, operation: &Operation, base: &str) -> Resource {
    if let Some((parameter, _, _)) =
        parameters(operation).find(|(p, _, _)| p.location == ParameterIn::Query && p.required)
    {
        panic!(
            "GET {path} cannot be an MCP resource: its query parameter `{}` is required, and a resource URI has no way to pass it; make it optional or expose the route as a tool",
            parameter.name
        );
    }
    let target = target(name, &Method::GET, path, operation);
    let template = !target.path_params.is_empty();
    // axum's `{id}` is already an RFC 6570 expression; `{*rest}` spans segments, which is
    // RFC 6570's reserved expansion `{+rest}`.
    let uri = format!("{base}{}", path.replace("{*", "{+"));
    let mut definition = Map::new();
    definition.insert(
        if template { "uriTemplate" } else { "uri" }.into(),
        uri.into(),
    );
    definition.insert("name".into(), target.name.clone().into());
    describe(operation, &mut definition);
    if let Some(mime_type) = success_media_type(operation) {
        definition.insert("mimeType".into(), mime_type.into());
    }
    Resource {
        target,
        definition: Value::Object(definition),
        template,
    }
}

fn prompt(name: String, path: &str, operation: &Operation) -> Prompt {
    let arguments: Vec<Value> = parameters(operation)
        .map(|(parameter, arg, _)| {
            let mut argument = Map::new();
            argument.insert("name".into(), arg.into());
            if let Some(description) = parameter_description(parameter) {
                argument.insert("description".into(), description.into());
            }
            argument.insert("required".into(), parameter.required.into());
            Value::Object(argument)
        })
        .collect();
    let target = target(name, &Method::GET, path, operation);
    let mut definition = Map::new();
    definition.insert("name".into(), target.name.clone().into());
    describe(operation, &mut definition);
    if !arguments.is_empty() {
        definition.insert("arguments".into(), arguments.into());
    }
    Prompt {
        target,
        definition: Value::Object(definition),
    }
}

fn tool(
    name: String,
    path: &str,
    method: &Method,
    operation: &Operation,
    schemas: &Map<String, Value>,
) -> Tool {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (parameter, arg, _) in parameters(operation) {
        let mut schema = parameter
            .schema
            .as_ref()
            .map(|s| s.clone().to_value())
            .unwrap_or_else(|| json!({}));
        if let (Some(description), Value::Object(object)) = (&parameter.description, &mut schema) {
            object.insert("description".into(), description.clone().into());
        }
        properties.insert(arg.clone(), schema);
        if parameter.required {
            required.push(arg);
        }
    }
    let mut target = target(name, method, path, operation);

    target.body = match &operation.request_body {
        None => BodyArgs::None,
        Some(body) => {
            let Some(schema) = body
                .content
                .get("application/json")
                .and_then(|m| m.schema.as_ref())
            else {
                panic!(
                    "{method} {path} cannot be an MCP tool: its request body is not JSON ({})",
                    body.content.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            };
            let schema = schema.clone().to_value();
            let resolved = resolve(&schema, schemas);
            let body_properties = resolved
                .get("properties")
                .and_then(Value::as_object)
                .filter(|_| resolved.get("type").and_then(Value::as_str) == Some("object"));
            match body_properties {
                Some(body_properties)
                    if body_properties.keys().all(|k| !properties.contains_key(k)) =>
                {
                    for (key, value) in body_properties {
                        properties.insert(key.clone(), value.clone());
                    }
                    // An optional body (`Option<Json<T>>`) makes none of its fields required.
                    if body.required
                        && let Some(Value::Array(body_required)) = resolved.get("required")
                    {
                        required.extend(
                            body_required
                                .iter()
                                .filter_map(Value::as_str)
                                .map(String::from),
                        );
                    }
                    BodyArgs::Flat
                }
                _ => {
                    properties.insert("body".into(), resolved.clone());
                    if body.required {
                        required.push("body".into());
                    }
                    BodyArgs::Wrapped {
                        required: body.required,
                    }
                }
            }
        }
    };

    let mut input = Map::new();
    input.insert("type".into(), "object".into());
    input.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        input.insert("required".into(), required.into());
    }
    let input = standalone(Value::Object(input), schemas);

    let output_schema = success_schema(operation).map(|schema| {
        // The root is inlined, not a `$ref`: the 2025 revisions require `"type": "object"` at the
        // top of an output schema, and clients drop a tool whose schema does not have it.
        let schema = resolve(&schema, schemas).clone();
        let is_object = schema.get("type").and_then(Value::as_str) == Some("object");
        (standalone(schema, schemas), is_object)
    });

    let mut definition = Map::new();
    definition.insert("name".into(), target.name.clone().into());
    describe(operation, &mut definition);
    definition.insert("inputSchema".into(), input);
    definition.insert(
        "annotations".into(),
        annotations(method, &operation.summary),
    );

    Tool {
        target,
        definition,
        output_schema,
    }
}

/// The JSON schema of the first 2xx response, if it has one.
fn success_schema(operation: &Operation) -> Option<Value> {
    operation
        .responses
        .iter()
        .filter(|(status, _)| status.starts_with('2'))
        .find_map(|(_, response)| {
            response
                .content
                .get("application/json")
                .and_then(|m| m.schema.as_ref())
        })
        .map(|schema| schema.clone().to_value())
}

/// The media type of the first 2xx response that has a body.
fn success_media_type(operation: &Operation) -> Option<String> {
    operation
        .responses
        .iter()
        .filter(|(status, _)| status.starts_with('2'))
        .find_map(|(_, response)| response.content.keys().next().cloned())
}

/// Hints derived from the method. MCP's defaults assume the worst (a tool may be destructive),
/// so every hint is spelled out.
fn annotations(method: &Method, summary: &Option<String>) -> Value {
    let read_only = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let mut annotations = Map::new();
    if let Some(summary) = summary {
        annotations.insert("title".into(), summary.clone().into());
    }
    annotations.insert("readOnlyHint".into(), read_only.into());
    if !read_only {
        // POST adds; PUT, PATCH and DELETE change or remove what exists.
        annotations.insert("destructiveHint".into(), (*method != Method::POST).into());
        annotations.insert(
            "idempotentHint".into(),
            matches!(*method, Method::PUT | Method::DELETE).into(),
        );
    }
    Value::Object(annotations)
}

/// `schema` itself, or the component it references.
fn resolve<'a>(schema: &'a Value, schemas: &'a Map<String, Value>) -> &'a Value {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix(COMPONENTS_PREFIX))
        .and_then(|name| schemas.get(name))
        .unwrap_or(schema)
}

/// `schema` with its `#/components/schemas/..` references pointing into a `$defs` of its own,
/// which holds every component it reaches: a tool's schema must stand alone.
fn standalone(schema: Value, schemas: &Map<String, Value>) -> Value {
    let mut reached = BTreeSet::new();
    let mut pending = Vec::new();
    collect_refs(&schema, &mut pending);
    while let Some(name) = pending.pop() {
        if reached.insert(name.clone())
            && let Some(component) = schemas.get(&name)
        {
            collect_refs(component, &mut pending);
        }
    }
    let mut schema = portable(rewrite_refs(schema));
    if !reached.is_empty()
        && let Value::Object(object) = &mut schema
    {
        let defs: Map<String, Value> = reached
            .into_iter()
            .filter_map(|name| {
                let component = schemas.get(&name)?.clone();
                Some((name, portable(rewrite_refs(component))))
            })
            .collect();
        object.insert("$defs".into(), Value::Object(defs));
    }
    schema
}

/// Keywords that describe a value rather than constrain it: they stay outside the `anyOf`.
const ANNOTATION_KEYWORDS: [&str; 7] = [
    "title",
    "description",
    "default",
    "deprecated",
    "examples",
    "readOnly",
    "writeOnly",
];

/// `{"type": ["string", "null"], "minLength": 1}` (what schemars writes for an `Option<T>`)
/// becomes `{"anyOf": [{"type": "string", "minLength": 1}, {"type": "null"}]}`. Both are JSON
/// Schema, but some model providers accept a single `type` only (Gemini's function
/// declarations), and the MCP Inspector's `--strict` flags the array form as not portable.
fn portable(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut object: Map<String, Value> = object
                .into_iter()
                .map(|(key, value)| (key, portable(value)))
                .collect();
            let nullable = match object.get("type") {
                Some(Value::Array(types)) if types.len() == 2 => {
                    let mut non_null = types.iter().filter(|t| t.as_str() != Some("null"));
                    match (non_null.next(), non_null.next()) {
                        (Some(single), None) => Some(single.clone()),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(single) = nullable {
                object.remove("type");
                let mut branch = Map::new();
                branch.insert("type".into(), single);
                let mut outer = Map::new();
                for (key, value) in object {
                    if ANNOTATION_KEYWORDS.contains(&key.as_str()) {
                        outer.insert(key, value);
                    } else {
                        branch.insert(key, value);
                    }
                }
                outer.insert(
                    "anyOf".into(),
                    json!([Value::Object(branch), {"type": "null"}]),
                );
                return Value::Object(outer);
            }
            Value::Object(object)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(portable).collect()),
        other => other,
    }
}

fn collect_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "$ref"
                    && let Some(name) = value
                        .as_str()
                        .and_then(|r| r.strip_prefix(COMPONENTS_PREFIX))
                {
                    out.push(name.to_string());
                } else {
                    collect_refs(value, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_refs(v, out)),
        _ => {}
    }
}

fn rewrite_refs(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| match (key.as_str(), &value) {
                    ("$ref", Value::String(r)) if r.starts_with(COMPONENTS_PREFIX) => {
                        let name = &r[COMPONENTS_PREFIX.len()..];
                        (key, Value::String(format!("{DEFS_PREFIX}{name}")))
                    }
                    _ => (key, rewrite_refs(value)),
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(rewrite_refs).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_carries_every_reached_component() {
        let schemas: Map<String, Value> = serde_json::from_value(json!({
            "Note": {"type": "object", "properties": {"tags": {"type": "array", "items": {"$ref": "#/components/schemas/Tag"}}}},
            "Tag": {"type": "string"},
            "Unused": {"type": "integer"},
        }))
        .unwrap();
        let schema = standalone(json!({"$ref": "#/components/schemas/Note"}), &schemas);
        assert_eq!(schema["$ref"], "#/$defs/Note");
        assert_eq!(
            schema["$defs"]["Note"]["properties"]["tags"]["items"]["$ref"],
            "#/$defs/Tag"
        );
        assert!(schema["$defs"].get("Unused").is_none());
    }

    #[test]
    fn nullable_types_become_any_of() {
        let schema = portable(json!({
            "type": "object",
            "properties": {
                "text": {"type": ["string", "null"], "minLength": 1, "description": "Text."},
                "either": {"type": ["string", "integer"]},
            },
        }));
        assert_eq!(
            schema["properties"]["text"],
            json!({"description": "Text.", "anyOf": [{"type": "string", "minLength": 1}, {"type": "null"}]})
        );
        // Only the `Option<T>` shape is rewritten.
        assert_eq!(
            schema["properties"]["either"]["type"],
            json!(["string", "integer"])
        );
    }

    #[test]
    fn annotations_follow_the_method() {
        let get = annotations(&Method::GET, &None);
        assert_eq!(get, json!({"readOnlyHint": true}));
        let post = annotations(&Method::POST, &None);
        assert_eq!(
            post,
            json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false})
        );
        let delete = annotations(&Method::DELETE, &None);
        assert_eq!(delete["destructiveHint"], true);
        assert_eq!(delete["idempotentHint"], true);
    }
}

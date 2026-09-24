//! Operations marked `mcp = "tool"` turned into MCP tools, from the OpenAPI document lesto
//! already builds: one input schema out of the path and query parameters and the JSON body, the
//! success response as the output schema, hints from the HTTP method.

use std::collections::{BTreeSet, HashMap};

use http::Method;
use serde_json::{Map, Value, json};

use crate::openapi::{OpenApi, Operation, ParameterIn};
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

#[derive(Debug, Clone)]
pub(crate) struct Tool {
    pub name: String,
    pub method: Method,
    /// Final path template, axum syntax.
    pub path: String,
    pub path_params: Vec<PathParam>,
    pub query_params: Vec<String>,
    pub body: BodyArgs,
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

#[derive(Debug, Default)]
pub(crate) struct Catalog {
    pub tools: Vec<Tool>,
    index: HashMap<String, usize>,
}

impl Catalog {
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.index.get(name).map(|&i| &self.tools[i])
    }

    /// The tools of `operations`, in registration order, documented from `spec` (which must
    /// have been built from the same operations).
    ///
    /// Panics, like axum on conflicting routes, when two operations ask for the same explicit
    /// tool name or a body is not JSON: both are mistakes in the application, found at startup.
    pub fn build(operations: &[(String, PendingOperation)], spec: &OpenApi) -> Catalog {
        let exposed: Vec<(&String, &PendingOperation, Option<&str>)> = operations
            .iter()
            .filter_map(|(path, pending)| {
                pending.meta.mcp.as_ref().map(|expose| match expose {
                    McpExpose::Tool { name } => (path, pending, name.as_deref()),
                })
            })
            .collect();

        // Explicit names first: they must be unique, and they win over a default name.
        let mut explicit: HashMap<&str, String> = HashMap::new();
        for (path, pending, name) in &exposed {
            if let Some(name) = name {
                let route = format!("{} {path}", pending.meta.method);
                if let Some(other) = explicit.insert(name, route.clone()) {
                    panic!(
                        "two routes are exposed as the MCP tool `{name}`: {other} and {route}; give one of them another `mcp(tool, name = \"..\")`"
                    );
                }
            }
        }
        let mut default_counts: HashMap<String, usize> = HashMap::new();
        for (_, pending, name) in &exposed {
            if name.is_none()
                && let Some(fn_name) = &pending.meta.name
            {
                *default_counts.entry(fn_name.clone()).or_default() += 1;
            }
        }

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
        for (path, pending, name) in exposed {
            let meta = &pending.meta;
            let operation_id = meta.default_operation_id(path);
            // The handler's name, unless another tool has it too: then the operationId, which
            // is unique by construction.
            let name = match (name, &meta.name) {
                (Some(name), _) => name.to_string(),
                (None, Some(fn_name))
                    if default_counts.get(fn_name) == Some(&1)
                        && !explicit.contains_key(fn_name.as_str()) =>
                {
                    fn_name.clone()
                }
                (None, _) => operation_id,
            };
            if catalog.index.contains_key(&name) {
                panic!(
                    "two routes are exposed as the MCP tool `{name}`; give one of them `mcp(tool, name = \"..\")`"
                );
            }
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
            let tool = tool(name, path, &meta.method, operation, &schemas);
            catalog.index.insert(tool.name.clone(), catalog.tools.len());
            catalog.tools.push(tool);
        }
        catalog
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
    let mut path_params = Vec::new();
    let mut query_params = Vec::new();

    for parameter in &operation.parameters {
        let location = parameter.location;
        if !matches!(location, ParameterIn::Path | ParameterIn::Query) {
            continue;
        }
        let wildcard = parameter.name.starts_with('*');
        let arg = parameter.name.trim_start_matches('*').to_string();
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
            required.push(arg.clone());
        }
        match location {
            ParameterIn::Path => path_params.push(PathParam {
                name: arg,
                wildcard,
            }),
            _ => query_params.push(arg),
        }
    }

    let body = match &operation.request_body {
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
                    properties.insert("body".into(), schema);
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
        let is_object = resolve(&schema, schemas)
            .get("type")
            .and_then(Value::as_str)
            == Some("object");
        (standalone(schema, schemas), is_object)
    });

    let mut definition = Map::new();
    definition.insert("name".into(), name.clone().into());
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
    definition.insert("inputSchema".into(), input);
    definition.insert(
        "annotations".into(),
        annotations(method, &operation.summary),
    );

    Tool {
        name,
        method: method.clone(),
        path: path.to_string(),
        path_params,
        query_params,
        body,
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
    let mut schema = rewrite_refs(schema);
    if !reached.is_empty()
        && let Value::Object(object) = &mut schema
    {
        let defs: Map<String, Value> = reached
            .into_iter()
            .filter_map(|name| {
                let component = schemas.get(&name)?.clone();
                Some((name, rewrite_refs(component)))
            })
            .collect();
        object.insert("$defs".into(), Value::Object(defs));
    }
    schema
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

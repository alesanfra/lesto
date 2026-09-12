//! A deliberately small OpenAPI 3.1 document model.
//!
//! Schemas are JSON Schema 2020-12 values produced by [`schemars`], which OpenAPI 3.1
//! embeds natively, so we keep them as [`schemars::Schema`] instead of re-modeling them.

use indexmap::IndexMap;
use schemars::Schema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApi {
    pub openapi: String,
    pub info: Info,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<Server>,
    pub paths: IndexMap<String, PathItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Components>,
    /// Default security requirements for every operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<SecurityRequirement>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
}

impl Default for OpenApi {
    fn default() -> Self {
        Self {
            openapi: "3.1.0".to_string(),
            info: Info::default(),
            servers: Vec::new(),
            paths: IndexMap::new(),
            components: None,
            security: None,
            tags: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub title: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Default for Info {
    fn default() -> Self {
        Self {
            title: "lesto".to_string(),
            version: "0.1.0".to_string(),
            description: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get: Option<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub put: Option<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<Operation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<Operation>,
}

impl PathItem {
    /// Mutable slot for the given HTTP method, or `None` for methods OpenAPI does not model.
    pub fn slot_mut(&mut self, method: &http::Method) -> Option<&mut Option<Operation>> {
        Some(match *method {
            http::Method::GET => &mut self.get,
            http::Method::PUT => &mut self.put,
            http::Method::POST => &mut self.post,
            http::Method::DELETE => &mut self.delete,
            http::Method::OPTIONS => &mut self.options,
            http::Method::HEAD => &mut self.head,
            http::Method::PATCH => &mut self.patch,
            http::Method::TRACE => &mut self.trace,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    pub responses: IndexMap<String, Response>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
    /// Alternative security requirements (any one satisfies). `Some(vec![])` disables auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<SecurityRequirement>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterIn {
    Path,
    Query,
    Header,
    Cookie,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "in")]
    pub location: ParameterIn,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub required: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Schema>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub content: IndexMap<String, MediaType>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaType {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Schema>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Response {
    pub description: String,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub content: IndexMap<String, MediaType>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Components {
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub schemas: IndexMap<String, Schema>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub security_schemes: IndexMap<String, SecurityScheme>,
}

/// `{"schemeName": ["scope", ...]}`; all listed schemes must be satisfied together.
pub type SecurityRequirement = IndexMap<String, Vec<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyIn {
    Header,
    Query,
    Cookie,
}

/// An OpenAPI security scheme object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SecurityScheme {
    ApiKey {
        name: String,
        #[serde(rename = "in")]
        location: ApiKeyIn,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Http {
        /// `bearer`, `basic`, or any IANA HTTP authentication scheme.
        scheme: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer_format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    #[serde(rename = "oauth2")]
    OAuth2 {
        flows: Box<OAuthFlows>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    OpenIdConnect {
        open_id_connect_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl SecurityScheme {
    pub fn bearer() -> Self {
        SecurityScheme::Http {
            scheme: "bearer".into(),
            bearer_format: None,
            description: None,
        }
    }

    pub fn bearer_jwt() -> Self {
        SecurityScheme::Http {
            scheme: "bearer".into(),
            bearer_format: Some("JWT".into()),
            description: None,
        }
    }

    pub fn basic() -> Self {
        SecurityScheme::Http {
            scheme: "basic".into(),
            bearer_format: None,
            description: None,
        }
    }

    pub fn api_key(location: ApiKeyIn, name: impl Into<String>) -> Self {
        SecurityScheme::ApiKey {
            name: name.into(),
            location,
            description: None,
        }
    }

    pub fn oauth2(flows: OAuthFlows) -> Self {
        SecurityScheme::OAuth2 {
            flows: Box::new(flows),
            description: None,
        }
    }

    pub fn open_id_connect(url: impl Into<String>) -> Self {
        SecurityScheme::OpenIdConnect {
            open_id_connect_url: url.into(),
            description: None,
        }
    }

    pub fn description(mut self, text: impl Into<String>) -> Self {
        let slot = match &mut self {
            SecurityScheme::ApiKey { description, .. }
            | SecurityScheme::Http { description, .. }
            | SecurityScheme::OAuth2 { description, .. }
            | SecurityScheme::OpenIdConnect { description, .. } => description,
        };
        *slot = Some(text.into());
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlows {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implicit: Option<OAuthFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<OAuthFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_credentials: Option<OAuthFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_code: Option<OAuthFlow>,
}

impl OAuthFlows {
    pub fn password(token_url: impl Into<String>) -> Self {
        Self {
            password: Some(OAuthFlow::new().token_url(token_url)),
            ..Self::default()
        }
    }

    pub fn client_credentials(token_url: impl Into<String>) -> Self {
        Self {
            client_credentials: Some(OAuthFlow::new().token_url(token_url)),
            ..Self::default()
        }
    }

    pub fn authorization_code(
        authorization_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        Self {
            authorization_code: Some(
                OAuthFlow::new()
                    .authorization_url(authorization_url)
                    .token_url(token_url),
            ),
            ..Self::default()
        }
    }

    pub fn implicit(authorization_url: impl Into<String>) -> Self {
        Self {
            implicit: Some(OAuthFlow::new().authorization_url(authorization_url)),
            ..Self::default()
        }
    }

    /// Declare a scope on every flow present.
    pub fn scope(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        let (name, description) = (name.into(), description.into());
        for flow in [
            &mut self.implicit,
            &mut self.password,
            &mut self.client_credentials,
            &mut self.authorization_code,
        ]
        .into_iter()
        .flatten()
        {
            flow.scopes.insert(name.clone(), description.clone());
        }
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthFlow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_url: Option<String>,
    /// Always present, possibly empty, as the spec requires.
    pub scopes: IndexMap<String, String>,
}

impl OAuthFlow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn authorization_url(mut self, url: impl Into<String>) -> Self {
        self.authorization_url = Some(url.into());
        self
    }

    pub fn token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url = Some(url.into());
        self
    }

    pub fn refresh_url(mut self, url: impl Into<String>) -> Self {
        self.refresh_url = Some(url.into());
        self
    }

    pub fn scope(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        self.scopes.insert(name.into(), description.into());
        self
    }
}

/// Convert an axum route template into an OpenAPI path template.
///
/// axum 0.8 already uses `{name}` for captures; only the wildcard form `{*rest}`
/// needs normalizing to `{rest}`.
pub fn openapi_path(axum_path: &str) -> String {
    axum_path.replace("{*", "{")
}

/// Names of the `{param}` placeholders in an OpenAPI path template, in order.
pub fn path_param_names(path: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                names.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_path_param_names() {
        assert_eq!(
            path_param_names("/users/{id}/posts/{post_id}"),
            vec!["id", "post_id"]
        );
        assert!(path_param_names("/health").is_empty());
    }

    #[test]
    fn normalises_wildcards() {
        assert_eq!(openapi_path("/files/{*path}"), "/files/{path}");
    }
}

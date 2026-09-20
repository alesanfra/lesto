use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use lesto::axum::body::Body;
use lesto::http::{Request, StatusCode, header};
use lesto::prelude::*;
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Db {
    users: Arc<Mutex<Vec<User>>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
struct User {
    id: u64,
    name: String,
    email: String,
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct CreateUser {
    /// Display name.
    #[garde(length(min = 1, max = 64))]
    name: String,
    #[garde(email)]
    email: String,
    #[garde(dive)]
    #[serde(default)]
    addresses: Vec<Address>,
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct Address {
    #[garde(length(min = 1))]
    city: String,
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct ListQuery {
    /// Page size.
    #[garde(range(min = 1, max = 100))]
    #[serde(default = "default_limit")]
    limit: u32,
    #[garde(skip)]
    #[allow(dead_code)]
    search: Option<String>,
}

fn default_limit() -> u32 {
    10
}

/// Create a user.
///
/// Users must have a **unique** email address.
#[lesto::post("/users", status = 201, tag = "users", responses(409))]
async fn create_user(
    State(db): State<Db>,
    Json(body): Json<CreateUser>,
) -> Result<Json<User>, HttpError> {
    let mut users = db.users.lock().unwrap();
    if users.iter().any(|u| u.email == body.email) {
        return Err(HttpError::conflict("email already taken"));
    }
    let user = User {
        id: users.len() as u64 + 1,
        name: body.name,
        email: body.email,
    };
    users.push(user.clone());
    Ok(Json(user))
}

#[lesto::get("/users", tag = "users")]
async fn list_users(State(db): State<Db>, Query(q): Query<ListQuery>) -> Json<Vec<User>> {
    let users = db.users.lock().unwrap();
    Json(users.iter().take(q.limit as usize).cloned().collect())
}

/// Get one user
#[lesto::get("/users/{id}", tag = "users", responses(404))]
async fn get_user(State(db): State<Db>, Path(id): Path<u64>) -> Result<Json<User>, HttpError> {
    db.users
        .lock()
        .unwrap()
        .iter()
        .find(|u| u.id == id)
        .cloned()
        .map(Json)
        .ok_or_else(|| HttpError::not_found("no such user"))
}

#[lesto::delete("/users/{id}", status = 204)]
async fn delete_user(Path(_id): Path<u64>) {}

#[lesto::get("/health", deprecated)]
async fn health() -> &'static str {
    "ok"
}

fn app() -> App<()> {
    App::new()
        .title("Test API")
        .version("2.0.0")
        .routes(routes![
            create_user,
            list_users,
            get_user,
            delete_user,
            health
        ])
        .with_state(Db::default())
}

async fn send(app: App<()>, req: Request<Body>) -> (StatusCode, Value) {
    let (status, _ct, body) = send_full(app, req).await;
    (status, body)
}

async fn send_full(app: App<()>, req: Request<Body>) -> (StatusCode, String, Value) {
    let response = app.into_router().oneshot(req).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, content_type, body)
}

/// `errors[]` entries as (in, pointer) pairs.
fn error_locations(problem: &Value) -> Vec<(String, String)> {
    problem["errors"]
        .as_array()
        .expect("errors array")
        .iter()
        .map(|e| {
            (
                e["in"].as_str().unwrap().to_owned(),
                e["pointer"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn json_request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn valid_body_is_accepted_and_status_overridden() {
    let (status, body) = send(
        app(),
        json_request(
            "POST",
            "/users",
            json!({"name": "Ada", "email": "ada@example.com"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body,
        json!({"id": 1, "name": "Ada", "email": "ada@example.com"})
    );
}

#[tokio::test]
async fn garde_failures_become_fastapi_422() {
    let (status, body) = send(
        app(),
        json_request(
            "POST",
            "/users",
            json!({"name": "", "email": "nope", "addresses": [{"city": "Rome"}, {"city": ""}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["type"], "about:blank");
    assert_eq!(body["title"], "Unprocessable Entity");
    assert_eq!(body["status"], 422);
    assert_eq!(body["instance"], "/users");
    assert_eq!(body["detail"], "3 validation errors");
    let locs = error_locations(&body);
    assert!(locs.contains(&("body".into(), "/name".into())), "{locs:?}");
    assert!(locs.contains(&("body".into(), "/email".into())), "{locs:?}");
    assert!(
        locs.contains(&("body".into(), "/addresses/1/city".into())),
        "{locs:?}"
    );
    assert!(
        body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["code"] == "value_error" && e["detail"].is_string())
    );
}

#[tokio::test]
async fn missing_field_is_reported_with_location() {
    let (status, body) = send(
        app(),
        json_request("POST", "/users", json!({"name": "Ada"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["errors"][0]["in"], "body");
    assert_eq!(body["errors"][0]["pointer"], "/email");
    assert_eq!(body["errors"][0]["code"], "missing");
}

#[tokio::test]
async fn malformed_json_is_422() {
    let req = Request::builder()
        .method("POST")
        .uri("/users")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let (status, content_type, body) = send_full(app(), req).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(body["errors"][0]["pointer"], "");
    assert_eq!(body["errors"][0]["code"], "json_invalid");
}

#[tokio::test]
async fn missing_content_type_is_415() {
    let req = Request::builder()
        .method("POST")
        .uri("/users")
        .body(Body::from("{}"))
        .unwrap();
    let (status, body) = send(app(), req).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["title"], "Unsupported Media Type");
    assert_eq!(body["status"], 415);
    assert!(body["detail"].is_string());
}

#[tokio::test]
async fn query_is_validated() {
    let (status, body) = send(app(), get("/users?limit=1000")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        error_locations(&body),
        vec![("query".to_string(), "/limit".to_string())]
    );

    let (status, body) = send(app(), get("/users?limit=abc")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        error_locations(&body),
        vec![("query".to_string(), "/limit".to_string())]
    );

    let (status, body) = send(app(), get("/users")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
}

#[tokio::test]
async fn path_parse_failure_is_422_and_http_error_is_detail() {
    let (status, body) = send(app(), get("/users/abc")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        error_locations(&body),
        vec![("path".to_string(), "/id".to_string())]
    );

    let (status, content_type, body) = send_full(app(), get("/users/42")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(
        body,
        json!({"type": "about:blank", "title": "Not Found", "status": 404, "detail": "no such user", "instance": "/users/42"})
    );
}

#[tokio::test]
async fn unit_return_with_204() {
    let req = Request::builder()
        .method("DELETE")
        .uri("/users/1")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(app(), req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
}

#[tokio::test]
async fn docs_pages_are_served() {
    let (status, body) = send(app(), get("/docs")).await;
    assert_eq!(status, StatusCode::OK);
    let html = body.as_str().unwrap();
    assert!(
        html.contains("@scalar/api-reference"),
        "Scalar is the default at /docs"
    );
    // Relative, so the pages work behind a prefix the app does not know about.
    assert!(html.contains(r#"url: "openapi.json""#), "{html}");
    let (status, body) = send(app(), get("/swagger")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_str().unwrap().contains("swagger-ui"));
    assert!(body.as_str().unwrap().contains(r#"url: "openapi.json""#));
    let (status, _) = send(app(), get("/scalar")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let custom = app()
        .openapi_url(Some("/spec/v1.json"))
        .docs_url(Some("/api/docs"));
    let (_, body) = send(custom, get("/api/docs")).await;
    assert!(body.as_str().unwrap().contains(r#"url: "../spec/v1.json""#));
}

#[tokio::test]
async fn openapi_document_is_derived_from_types() {
    let (status, spec) = send(app(), get("/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(spec["openapi"], "3.1.0");
    assert_eq!(spec["info"]["title"], "Test API");
    assert_eq!(spec["info"]["version"], "2.0.0");

    let post = &spec["paths"]["/users"]["post"];
    assert_eq!(post["summary"], "Create a user.");
    assert_eq!(
        post["description"],
        "Users must have a **unique** email address."
    );
    assert_eq!(post["operationId"], "create_user_users_post");
    assert_eq!(post["tags"], json!(["users"]));
    assert_eq!(post["requestBody"]["required"], true);
    assert_eq!(
        post["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/CreateUser"
    );
    assert_eq!(
        post["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/User"
    );
    assert_eq!(
        post["responses"]["422"]["content"]["application/problem+json"]["schema"]["$ref"],
        "#/components/schemas/Problem"
    );
    assert_eq!(
        post["responses"]["409"]["content"]["application/problem+json"]["schema"]["$ref"],
        "#/components/schemas/Problem"
    );
    assert_eq!(
        post["responses"]["default"]["content"]["application/problem+json"]["schema"]["$ref"],
        "#/components/schemas/Problem"
    );
    let keys: Vec<&String> = post["responses"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["201", "409", "422", "default"]);
    assert!(post["responses"].get("200").is_none());

    let list = &spec["paths"]["/users"]["get"];
    assert_eq!(list["summary"], "List users");
    let params = list["parameters"].as_array().unwrap();
    let limit = params
        .iter()
        .find(|p| p["name"] == "limit")
        .expect("limit param");
    assert_eq!(limit["in"], "query");
    assert_eq!(limit["required"], false);
    assert_eq!(limit["description"], "Page size.");
    assert_eq!(limit["schema"]["type"], "integer");
    let search = params
        .iter()
        .find(|p| p["name"] == "search")
        .expect("search param");
    assert_eq!(search["required"], false);
    assert_eq!(
        list["responses"]["200"]["content"]["application/json"]["schema"]["type"],
        "array"
    );
    assert_eq!(
        list["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
        "#/components/schemas/User"
    );

    let get_one = &spec["paths"]["/users/{id}"]["get"];
    assert_eq!(get_one["summary"], "Get one user");
    let id = &get_one["parameters"][0];
    assert_eq!(id["name"], "id");
    assert_eq!(id["in"], "path");
    assert_eq!(id["required"], true);
    assert_eq!(id["schema"]["type"], "integer");
    assert_eq!(get_one["responses"]["404"]["description"], "Not Found");

    let del = &spec["paths"]["/users/{id}"]["delete"];
    assert_eq!(del["responses"]["204"]["description"], "No Content");
    assert!(del["responses"]["204"].get("content").is_none());

    let health = &spec["paths"]["/health"]["get"];
    assert_eq!(health["deprecated"], true);
    assert!(health["responses"]["200"]["content"]["text/plain; charset=utf-8"].is_object());

    let schemas = &spec["components"]["schemas"];
    assert_eq!(
        schemas["CreateUser"]["properties"]["name"]["description"],
        "Display name."
    );
    assert_eq!(schemas["CreateUser"]["required"], json!(["name", "email"]));
    assert!(schemas["Address"].is_object());
    assert!(schemas["User"].is_object());
    assert!(schemas["Problem"].is_object());
    assert!(schemas["ProblemError"].is_object());
    assert!(schemas.get("HttpErrorBody").is_none());

    // docs routes never leak into the spec
    assert!(spec["paths"].get("/docs").is_none());
    assert!(spec["paths"].get("/openapi.json").is_none());
}

#[tokio::test]
async fn nested_apps_merge_docs_with_prefix() {
    let api = App::<Db>::new().routes(routes![get_user, health]);
    let root = App::new()
        .title("Root")
        .route(lesto::get("/ping").summary("Ping"), || async { "pong" })
        .nest("/api/v1", api)
        .with_state(Db::default());
    let spec = root.openapi();
    let spec = serde_json::to_value(spec).unwrap();
    assert!(spec["paths"]["/api/v1/users/{id}"]["get"].is_object());
    assert!(spec["paths"]["/api/v1/health"]["get"].is_object());
    assert_eq!(spec["paths"]["/ping"]["get"]["summary"], "Ping");
    assert!(spec["components"]["schemas"]["User"].is_object());

    let (status, body) = send(root, get("/api/v1/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn docs_can_be_disabled() {
    let app = App::new()
        .openapi_url(None)
        .routes(routes![health])
        .with_state(Db::default());
    let (status, _) = send(app, get("/docs")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fastapi_error_format_is_available() {
    fn fastapi_app() -> App<()> {
        App::new()
            .error_format(ErrorFormat::FastApi)
            .routes(routes![create_user, get_user])
            .with_state(Db::default())
    }

    let (status, content_type, body) = send_full(fastapi_app(), get("/users/42")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(content_type, "application/json");
    assert_eq!(body, json!({"detail": "no such user"}));

    let (status, body) = send(
        fastapi_app(),
        json_request(
            "POST",
            "/users",
            json!({"name": "", "email": "ada@example.com", "addresses": [{"city": ""}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let detail = body["detail"].as_array().unwrap();
    let locs: Vec<Value> = detail.iter().map(|d| d["loc"].clone()).collect();
    assert!(locs.contains(&json!(["body", "name"])), "{locs:?}");
    assert!(
        locs.contains(&json!(["body", "addresses", 0, "city"])),
        "{locs:?}"
    );
    assert_eq!(detail[0]["type"], "value_error");

    let spec = serde_json::to_value(fastapi_app().openapi()).unwrap();
    let post = &spec["paths"]["/users"]["post"];
    assert_eq!(
        post["responses"]["422"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/HTTPValidationError"
    );
    assert_eq!(
        post["responses"]["409"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/HttpErrorBody"
    );
}

#[tokio::test]
async fn problem_type_and_extensions_are_kept() {
    #[lesto::get("/teapot")]
    async fn teapot() -> Result<(), HttpError> {
        Err(HttpError::new(418, "short and stout")
            .with_type("https://example.com/problems/teapot")
            .with_title("I'm a teapot")
            .with_extension("volume_ml", 500))
    }
    let app = App::new().routes(routes![teapot]);
    let (status, body) = send(app, get("/teapot")).await;
    assert_eq!(status, StatusCode::IM_A_TEAPOT);
    assert_eq!(
        body,
        json!({
            "type": "https://example.com/problems/teapot",
            "title": "I'm a teapot",
            "status": 418,
            "detail": "short and stout",
            "instance": "/teapot",
            "volume_ml": 500
        })
    );
}

#[tokio::test]
async fn error_format_can_be_set_after_routes() {
    let app: App<()> = App::new()
        .routes(routes![get_user])
        .error_format(ErrorFormat::FastApi)
        .with_state(Db::default());
    let spec = serde_json::to_value(app.openapi()).unwrap();
    assert!(
        spec["paths"]["/users/{id}"]["get"]["responses"]["404"]["content"]["application/json"]
            .is_object()
    );
}

// ---- security ------------------------------------------------------------------------------

struct TenantKey;
impl ApiKeyScheme for TenantKey {
    const NAME: &'static str = "tenantKey";
    const KEY: &'static str = "X-Tenant-Key";
}

struct SessionCookie;
impl ApiKeyScheme for SessionCookie {
    const NAME: &'static str = "session";
    const KEY: &'static str = "sid";
    const LOCATION: ApiKeyIn = ApiKeyIn::Cookie;
}

struct OAuth2;
impl AuthScheme for OAuth2 {
    const NAME: &'static str = "oauth2";
    fn scheme() -> SecurityScheme {
        SecurityScheme::oauth2(
            OAuthFlows::password("/token")
                .scope("read", "Read things")
                .scope("write", "Write things"),
        )
    }
    fn scopes() -> &'static [&'static str] {
        &["read"]
    }
}

#[lesto::get("/me")]
async fn me(auth: Bearer) -> String {
    format!("token={}", auth.token())
}

#[lesto::get("/basic")]
async fn basic(auth: Basic) -> String {
    format!("{}:{}", auth.username, auth.password)
}

#[lesto::get("/tenant")]
async fn tenant(key: ApiKey<TenantKey>) -> String {
    key.into_key()
}

#[lesto::get("/session")]
async fn session(key: ApiKey<SessionCookie>) -> String {
    key.into_key()
}

#[lesto::get("/oauth")]
async fn oauth(auth: Bearer<OAuth2>) -> String {
    auth.into_token()
}

#[lesto::get("/middleware-protected", security(bearerAuth), responses(403))]
async fn middleware_protected() -> &'static str {
    "ok"
}

#[lesto::get("/scoped", security(oauth2 = ["write"], "tenantKey"))]
async fn scoped() -> &'static str {
    "ok"
}

#[lesto::get("/public", public)]
async fn public_route() -> &'static str {
    "ok"
}

fn secure_app() -> App<()> {
    App::new()
        .security_scheme("bearerAuth", SecurityScheme::bearer_jwt())
        .routes(routes![
            me,
            basic,
            tenant,
            session,
            oauth,
            middleware_protected,
            scoped,
            public_route
        ])
}

fn with_header(uri: &str, name: &str, value: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(name, value)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn bearer_extracts_token_or_401_with_challenge() {
    let (status, body) = send(
        secure_app(),
        with_header("/me", "authorization", "bearer abc.def"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "token=abc.def");

    let response = secure_app()
        .into_router()
        .oneshot(get("/me"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );

    let (status, _) = send(
        secure_app(),
        with_header("/me", "authorization", "Basic abc"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn basic_decodes_credentials() {
    // "ada:secret"
    let (status, body) = send(
        secure_app(),
        with_header("/basic", "authorization", "Basic YWRhOnNlY3JldA=="),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ada:secret");

    let (status, body) = send(
        secure_app(),
        with_header("/basic", "authorization", "Basic !!!"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["detail"], "Invalid authentication credentials");
}

#[tokio::test]
async fn api_key_from_header_and_cookie() {
    let (status, body) = send(secure_app(), with_header("/tenant", "x-tenant-key", "t-1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "t-1");
    let (status, _) = send(secure_app(), get("/tenant")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = send(
        secure_app(),
        with_header("/session", "cookie", "theme=dark; sid=s-9"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "s-9");
}

#[tokio::test]
async fn security_schemes_and_requirements_are_documented() {
    let spec = serde_json::to_value(secure_app().openapi()).unwrap();
    let schemes = &spec["components"]["securitySchemes"];
    // Registered up front, so the JWT format wins over the extractor's default.
    assert_eq!(
        schemes["bearerAuth"],
        json!({"type": "http", "scheme": "bearer", "bearerFormat": "JWT"})
    );
    assert_eq!(
        schemes["basicAuth"],
        json!({"type": "http", "scheme": "basic"})
    );
    assert_eq!(
        schemes["tenantKey"],
        json!({"type": "apiKey", "name": "X-Tenant-Key", "in": "header"})
    );
    assert_eq!(
        schemes["session"],
        json!({"type": "apiKey", "name": "sid", "in": "cookie"})
    );
    assert_eq!(
        schemes["oauth2"],
        json!({
            "type": "oauth2",
            "flows": {"password": {"tokenUrl": "/token", "scopes": {"read": "Read things", "write": "Write things"}}}
        })
    );

    let me = &spec["paths"]["/me"]["get"];
    assert_eq!(me["security"], json!([{"bearerAuth": []}]));
    assert_eq!(
        me["responses"]["401"]["content"]["application/problem+json"]["schema"]["$ref"],
        "#/components/schemas/Problem"
    );
    assert_eq!(
        spec["paths"]["/oauth"]["get"]["security"],
        json!([{"oauth2": ["read"]}])
    );
    assert_eq!(
        spec["paths"]["/middleware-protected"]["get"]["security"],
        json!([{"bearerAuth": []}])
    );
    assert_eq!(
        spec["paths"]["/scoped"]["get"]["security"],
        json!([{"oauth2": ["write"]}, {"tenantKey": []}])
    );
    assert_eq!(spec["paths"]["/public"]["get"]["security"], json!([]));
    assert!(spec["paths"]["/basic"]["get"]["security"].is_array());
    assert!(spec.get("security").is_none());
}

#[tokio::test]
async fn app_wide_security_is_the_default() {
    let app = App::<()>::new()
        .security_scheme("bearerAuth", SecurityScheme::bearer())
        .security("bearerAuth", &[])
        .routes(routes![public_route, middleware_protected]);
    let spec = serde_json::to_value(app.openapi()).unwrap();
    assert_eq!(spec["security"], json!([{"bearerAuth": []}]));
    assert_eq!(spec["paths"]["/public"]["get"]["security"], json!([]));
}

#[tokio::test]
async fn boolean_schemas_are_rendered_as_objects() {
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Envelope {
        payload: serde_json::Value,
    }

    #[lesto::get("/any")]
    async fn any() -> Json<serde_json::Value> {
        Json(json!({}))
    }

    #[lesto::post("/envelope")]
    async fn envelope(body: lesto::axum::Json<Envelope>) -> Json<Envelope> {
        Json(body.0)
    }

    let spec =
        serde_json::to_value(App::<()>::new().routes(routes![any, envelope]).openapi()).unwrap();
    assert_eq!(
        spec["paths"]["/any"]["get"]["responses"]["200"]["content"]["application/json"]["schema"],
        json!({})
    );
    assert_eq!(
        spec["components"]["schemas"]["Envelope"]["properties"]["payload"],
        json!({})
    );
}

#[tokio::test]
async fn schema_properties_keep_declaration_order() {
    let spec = serde_json::to_value(app().openapi()).unwrap();
    let keys = |schema: &str| -> Vec<String> {
        spec["components"]["schemas"][schema]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(keys("User"), ["id", "name", "email"]);
    assert_eq!(keys("CreateUser"), ["name", "email", "addresses"]);
    assert_eq!(
        keys("Problem"),
        ["type", "title", "status", "detail", "instance", "errors"]
    );
}

#[tokio::test]
async fn nested_root_route_is_documented_where_axum_serves_it() {
    #[lesto::get("/")]
    async fn index() -> &'static str {
        "users index"
    }
    let root = App::new().nest("/users", App::<()>::new().routes(routes![index]));
    let spec = serde_json::to_value(root.openapi()).unwrap();
    assert!(
        spec["paths"]["/users"]["get"].is_object(),
        "{}",
        spec["paths"]
    );
    assert!(spec["paths"].get("/users/").is_none());

    let (status, body) = send(root, get("/users")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "users index");
}

// ---- #[lesto::views] ------------------------------------------------------------------------

#[lesto::views(Create(author, text), Update(text?, pinned?))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
struct Note {
    #[garde(skip)]
    id: u64,
    /// Who wrote it.
    #[garde(length(min = 1, max = 64))]
    author: String,
    #[garde(length(min = 1))]
    text: String,
    #[garde(skip)]
    #[serde(default)]
    is_pinned: bool,
    #[garde(skip)]
    #[serde(default)]
    pinned: bool,
}

#[lesto::post("/notes", status = 201)]
async fn create_note(Json(body): Json<NoteCreate>) -> Json<Note> {
    let mut note = Note {
        id: 7,
        author: String::new(),
        text: String::new(),
        is_pinned: false,
        pinned: false,
    };
    note.apply_create(body);
    Json(note)
}

#[lesto::patch("/notes/{id}")]
async fn update_note(Path(id): Path<u64>, Json(body): Json<NoteUpdate>) -> Json<Note> {
    let mut note = Note {
        id,
        author: "ada".into(),
        text: "old".into(),
        is_pinned: false,
        pinned: false,
    };
    note.apply_update(body);
    Json(note)
}

fn notes_app() -> App<()> {
    App::<()>::new().routes(routes![create_note, update_note])
}

#[tokio::test]
async fn views_generate_structs_with_inherited_attributes() {
    // Create view: both fields required, garde rules kept, camelCase kept.
    let (status, body) = send(
        notes_app(),
        json_request("POST", "/notes", json!({"author": "Ada", "text": "hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body,
        json!({"id": 7, "author": "Ada", "text": "hi", "isPinned": false, "pinned": false})
    );

    let (status, body) = send(
        notes_app(),
        json_request("POST", "/notes", json!({"author": "", "text": "hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["errors"][0]["pointer"], "/author");

    let (status, body) = send(
        notes_app(),
        json_request("POST", "/notes", json!({"author": "Ada"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["errors"][0]["pointer"], "/text");
}

#[tokio::test]
async fn optional_view_fields_apply_only_when_present() {
    let (status, body) = send(
        notes_app(),
        json_request("PATCH", "/notes/3", json!({"pinned": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "old");
    assert_eq!(body["pinned"], true);

    let (status, body) = send(
        notes_app(),
        json_request("PATCH", "/notes/3", json!({"text": "new"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "new");

    // garde rule wrapped in inner(): an empty text is still rejected when present.
    let (status, body) = send(
        notes_app(),
        json_request("PATCH", "/notes/3", json!({"text": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["errors"][0]["pointer"], "/text");
}

#[test]
fn views_are_documented_as_separate_schemas() {
    let spec = serde_json::to_value(notes_app().openapi()).unwrap();
    let schemas = &spec["components"]["schemas"];
    let keys = |name: &str| -> Vec<String> {
        schemas[name]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    };
    assert_eq!(keys("Note"), ["id", "author", "text", "isPinned", "pinned"]);
    assert_eq!(keys("NoteCreate"), ["author", "text"]);
    assert_eq!(schemas["NoteCreate"]["required"], json!(["author", "text"]));
    assert_eq!(
        schemas["NoteCreate"]["properties"]["author"]["description"],
        "Who wrote it."
    );
    assert_eq!(
        schemas["NoteCreate"]["properties"]["author"]["maxLength"],
        64
    );
    assert_eq!(keys("NoteUpdate"), ["text", "pinned"]);
    assert!(schemas["NoteUpdate"].get("required").is_none());
    assert_eq!(
        spec["paths"]["/notes"]["post"]["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/NoteCreate"
    );
}

// ---- production hardening ---------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct TagQuery {
    #[garde(skip)]
    #[serde(default)]
    tag: Vec<String>,
}

#[lesto::get("/tags")]
async fn tags(Query(q): Query<TagQuery>) -> String {
    q.tag.join(",")
}

#[tokio::test]
async fn repeated_query_parameters_collect_into_a_vec() {
    let app = || App::<()>::new().routes(routes![tags]);
    let (status, body) = send(app(), get("/tags?tag=a&tag=b")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "a,b");
    let (status, body) = send(app(), get("/tags")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let spec = serde_json::to_value(app().openapi()).unwrap();
    let param = &spec["paths"]["/tags"]["get"]["parameters"][0];
    assert_eq!(param["name"], "tag");
    assert_eq!(param["schema"]["type"], "array");
}

#[tokio::test]
async fn unknown_route_is_a_problem_404() {
    let (status, content_type, body) = send_full(app(), get("/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(body["title"], "Not Found");
    assert_eq!(body["status"], 404);
    assert_eq!(body["instance"], "/nope");

    let (status, content_type, body) =
        send_full(app().error_format(ErrorFormat::FastApi), get("/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(content_type, "application/json");
    assert_eq!(body, json!({"detail": "Not Found"}));
}

#[tokio::test]
async fn wrong_method_is_a_problem_405() {
    let req = Request::builder()
        .method("PATCH")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let (status, content_type, body) = send_full(app(), req).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(body["title"], "Method Not Allowed");
    assert_eq!(body["instance"], "/health");
}

#[lesto::get("/boom")]
async fn boom() -> &'static str {
    panic!("something went very wrong")
}

#[tokio::test]
async fn panicking_handler_answers_500_problem() {
    let app = App::<()>::new().routes(routes![boom]);
    let (status, content_type, body) = send_full(app, get("/boom")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(body["detail"], "Internal Server Error");
    assert_eq!(body["instance"], "/boom");
    assert!(
        !body.to_string().contains("very wrong"),
        "panic message must not leak: {body}"
    );
}

mod ids_a {
    #[lesto::get("/a")]
    pub async fn list() -> &'static str {
        "a"
    }
}

mod ids_b {
    use lesto::prelude::*;
    #[lesto::get("/b/{id}")]
    pub async fn list(Path(_id): Path<u64>) -> &'static str {
        "b"
    }
    #[lesto::post("/b", operation_id = "make_b")]
    pub async fn create() -> &'static str {
        "b"
    }
}

#[test]
fn operation_ids_are_unique_per_path_and_method() {
    let inner = App::<()>::new().routes(routes![ids_b::list, ids_b::create]);
    let app = App::<()>::new()
        .routes(routes![ids_a::list])
        .nest("/api", inner);
    let spec = serde_json::to_value(app.openapi()).unwrap();
    assert_eq!(spec["paths"]["/a"]["get"]["operationId"], "list_a_get");
    assert_eq!(
        spec["paths"]["/api/b/{id}"]["get"]["operationId"], "list_api_b__id__get",
        "the final path (prefix included) is part of the id, as in FastAPI"
    );
    assert_eq!(
        spec["paths"]["/api/b"]["post"]["operationId"], "make_b",
        "an explicit operation_id is kept verbatim"
    );
    // Hand-built routes have no function name: method and path alone.
    let manual = App::<()>::new().route(lesto::get("/ping"), || async { "pong" });
    let spec = serde_json::to_value(manual.openapi()).unwrap();
    assert_eq!(spec["paths"]["/ping"]["get"]["operationId"], "ping_get");
}

#[lesto::get("/debug/bearer")]
async fn debug_bearer(auth: Bearer) -> String {
    format!("{auth:?}")
}

#[lesto::get("/debug/basic")]
async fn debug_basic(auth: Basic) -> String {
    format!("{auth:?}")
}

struct DebugKey;
impl ApiKeyScheme for DebugKey {
    const NAME: &'static str = "debugKey";
    const KEY: &'static str = "X-Debug-Key";
}

#[lesto::get("/debug/key")]
async fn debug_key(key: ApiKey<DebugKey>) -> String {
    format!("{key:?}")
}

#[tokio::test]
async fn credentials_are_redacted_in_debug_output() {
    let app = || App::<()>::new().routes(routes![debug_bearer, debug_basic, debug_key]);
    let (_, body) = send(
        app(),
        with_header("/debug/bearer", "authorization", "Bearer s3cret-token"),
    )
    .await;
    let text = body.as_str().unwrap();
    assert!(text.starts_with("Bearer"), "{text}");
    assert!(!text.contains("s3cret"), "token leaked: {text}");

    // ada:hunter2
    let (_, body) = send(
        app(),
        with_header("/debug/basic", "authorization", "Basic YWRhOmh1bnRlcjI="),
    )
    .await;
    let text = body.as_str().unwrap();
    assert!(text.contains("ada"), "the username is not secret: {text}");
    assert!(!text.contains("hunter2"), "password leaked: {text}");

    let (_, body) = send(app(), with_header("/debug/key", "x-debug-key", "k-12345")).await;
    let text = body.as_str().unwrap();
    assert!(!text.contains("12345"), "key leaked: {text}");
}

/// Serializes to an error, as a `Mutex`-poisoned or otherwise broken value might.
#[derive(JsonSchema)]
struct Unserializable;

impl Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("database password is hunter2"))
    }
}

#[lesto::get("/unserializable")]
async fn unserializable() -> Json<Unserializable> {
    Json(Unserializable)
}

#[tokio::test]
async fn json_serialization_failure_is_a_500_without_details() {
    let app = App::<()>::new().routes(routes![unserializable]);
    let (status, body) = send(app, get("/unserializable")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["detail"], "Internal Server Error");
    assert!(!body.to_string().contains("hunter2"), "{body}");
}

#[lesto::get("/huge-problem")]
async fn huge_problem() -> HttpError {
    HttpError::bad_request("x".repeat(2 << 20))
}

#[tokio::test]
async fn large_problem_bodies_still_get_an_instance() {
    let app = App::<()>::new().routes(routes![huge_problem]);
    let response = app
        .into_router()
        .oneshot(get("/huge-problem"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let declared = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .map(|v| v.to_str().unwrap().parse::<usize>().unwrap());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    if let Some(declared) = declared {
        assert_eq!(declared, bytes.len(), "Content-Length must match the body");
    }
    let problem: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(problem["instance"], "/huge-problem");
    assert_eq!(problem["detail"].as_str().unwrap().len(), 2 << 20);
}

#[tokio::test]
async fn docs_pages_pin_their_assets_and_carry_integrity_hashes() {
    let (_, body) = send(app(), get("/docs")).await;
    let html = body.as_str().unwrap();
    assert!(
        html.contains("https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.68.0"),
        "{html}"
    );
    assert!(html.contains(r#"integrity="sha384-"#), "{html}");
    let (_, body) = send(app(), get("/swagger")).await;
    let html = body.as_str().unwrap();
    assert!(
        html.contains("https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.32.15/swagger-ui.css"),
        "{html}"
    );
    assert!(
        html.contains("https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.32.15/swagger-ui-bundle.js"),
        "{html}"
    );
    assert_eq!(html.matches(r#"integrity="sha384-"#).count(), 2);

    // Self-hosted assets: the URLs are used as given and no integrity is claimed.
    let custom = app()
        .scalar_script_url("https://cdn.internal/scalar.js")
        .swagger_ui_base_url("https://cdn.internal/swagger-ui/");
    let (_, body) = send(custom, get("/docs")).await;
    let html = body.as_str().unwrap();
    assert!(
        html.contains(r#"src="https://cdn.internal/scalar.js""#),
        "{html}"
    );
    assert!(!html.contains("integrity="), "{html}");
    let custom = app().swagger_ui_base_url("https://cdn.internal/swagger-ui");
    let (_, body) = send(custom, get("/swagger")).await;
    let html = body.as_str().unwrap();
    assert!(
        html.contains(r#"href="https://cdn.internal/swagger-ui/swagger-ui.css""#),
        "{html}"
    );
    assert!(
        html.contains(r#"src="https://cdn.internal/swagger-ui/swagger-ui-bundle.js""#),
        "{html}"
    );
    assert!(!html.contains("integrity="), "{html}");
}

// ---- problem rendering ---------------------------------------------------------------------

/// A response built by hand, not through `Problem`: the marker is absent, so `ProblemLayer`
/// takes its slow path (buffer, parse, rewrite).
#[lesto::get("/hand-rolled", responses(409))]
async fn hand_rolled() -> lesto::axum::response::Response {
    lesto::axum::response::Response::builder()
        .status(StatusCode::CONFLICT)
        .header(header::CONTENT_TYPE, "application/problem+json")
        .body(Body::from(
            r#"{"type":"about:blank","title":"Conflict","status":409,"detail":"by hand"}"#,
        ))
        .unwrap()
}

#[tokio::test]
async fn a_hand_built_problem_is_still_finished() {
    let app = App::<()>::new().routes(routes![hand_rolled]);
    let (status, content_type, body) = send_full(app, get("/hand-rolled")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(body["instance"], "/hand-rolled");
    assert_eq!(body["detail"], "by hand");

    let app = App::<()>::new()
        .routes(routes![hand_rolled])
        .error_format(ErrorFormat::FastApi);
    let (status, content_type, body) = send_full(app, get("/hand-rolled")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(content_type, "application/json");
    assert_eq!(body, json!({"detail": "by hand"}));
}

/// Built where the layer's request context cannot be seen. It must come out like any other.
#[lesto::get("/spawned", responses(404))]
async fn spawned() -> lesto::axum::response::Response {
    use lesto::axum::response::IntoResponse;
    tokio::spawn(async { HttpError::not_found("from another task").into_response() })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_problem_built_in_a_spawned_task_is_finished_too() {
    let app = App::<()>::new().routes(routes![spawned]);
    let (status, content_type, body) = send_full(app, get("/spawned")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(body["instance"], "/spawned");
    assert_eq!(body["detail"], "from another task");
}

/// A `problem+json` body that is not a problem document at all is passed through unchanged.
#[lesto::get("/not-a-problem", responses(400))]
async fn not_a_problem() -> lesto::axum::response::Response {
    lesto::axum::response::Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header(header::CONTENT_TYPE, "application/problem+json")
        .body(Body::from("not json at all"))
        .unwrap()
}

#[tokio::test]
async fn an_unparsable_problem_body_is_left_alone() {
    let app = App::<()>::new().routes(routes![not_a_problem]);
    let response = app
        .into_router()
        .oneshot(get("/not-a-problem"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"not json at all");
}

#[tokio::test]
async fn the_problem_layer_works_on_a_plain_axum_router() {
    use lesto::layers::ProblemLayer;

    let router = lesto::axum::Router::new()
        .route(
            "/gone",
            lesto::axum::routing::get(|| async { HttpError::not_found("gone") }),
        )
        .layer(ProblemLayer::new());
    let response = router.oneshot(get("/gone")).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let problem: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(problem["instance"], "/gone", "the layer alone is enough");
}

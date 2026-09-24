//! `lesto::mcp` end to end through `oneshot`: both protocol eras, the tool catalog, tool calls
//! as requests to the app's own routes, errors, authentication challenges, Origin checks.

use http_body_util::BodyExt;
use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::http::{HeaderMap, Request, StatusCode, header};
use lesto::mcp::{Mcp, McpCall, Prompt};
use lesto::prelude::*;
use serde_json::{Value, json};
use tower::ServiceExt;

const MODERN: &str = "2026-07-28";

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct NoteCreate {
    /// The note's title.
    #[garde(length(min = 1))]
    title: String,
    #[garde(skip)]
    body: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Note {
    id: u64,
    title: String,
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct Search {
    /// Words to look for.
    #[garde(length(min = 1))]
    q: String,
    #[garde(skip)]
    tag: Option<Vec<String>>,
}

/// Create a note.
///
/// Titles need not be unique.
#[lesto::post("/notes", status = 201, mcp = "tool")]
async fn create_note(Json(new): Json<NoteCreate>) -> Json<Note> {
    let _ = new.body;
    Json(Note {
        id: 1,
        title: new.title,
    })
}

/// Fetch a note.
#[lesto::get("/notes/{id}", responses(404), mcp = "tool")]
async fn get_note(Path(id): Path<u64>) -> Result<Json<Note>, HttpError> {
    if id == 1 {
        Ok(Json(Note {
            id,
            title: "first".into(),
        }))
    } else {
        Err(HttpError::not_found(format!("note {id} not found")))
    }
}

/// Search the notes.
#[lesto::get("/notes/search", mcp(tool, name = "search_notes"))]
async fn search(Query(search): Query<Search>) -> Json<Vec<String>> {
    let mut found = vec![search.q];
    found.extend(search.tag.unwrap_or_default());
    Json(found)
}

/// Who is calling.
#[lesto::get("/whoami", mcp = "tool")]
async fn whoami(call: Option<Extension<McpCall>>, auth: Bearer) -> String {
    let via = call.map(|Extension(c)| c.name().to_string());
    format!("{} via {}", auth.token(), via.unwrap_or_default())
}

/// Not exposed.
#[lesto::delete("/notes/{id}")]
async fn delete_note(Path(_id): Path<u64>) -> StatusCode {
    StatusCode::NO_CONTENT
}

fn app(mcp: Mcp) -> Router {
    App::new()
        .title("Notes API")
        .version("1.2.3")
        .routes(routes![create_note, search, get_note, whoami, delete_note])
        .mcp(mcp.instructions("Notes, searchable."))
        .into_router()
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

async fn send(router: Router, headers: &[(&str, &str)], body: Value) -> Reply {
    let mut request = Request::post("/mcp").header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{e}: {bytes:?}"))
    };
    Reply {
        status,
        headers,
        body,
    }
}

/// A 2026-07-28 request, with the headers its body implies.
async fn modern(router: Router, method: &str, params: Value) -> Reply {
    modern_with(router, method, params, &[]).await
}

async fn modern_with(
    router: Router,
    method: &str,
    mut params: Value,
    extra: &[(&str, &str)],
) -> Reply {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let name = params.get("name").and_then(Value::as_str).map(String::from);
    let mut headers = vec![("mcp-protocol-version", MODERN), ("mcp-method", method)];
    if let Some(name) = &name {
        headers.push(("mcp-name", name));
    }
    headers.extend_from_slice(extra);
    let body = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
    send(router, &headers, body).await
}

/// A 2025-11-25 request (after `initialize`: the version is in the header).
async fn legacy(router: Router, method: &str, params: Value) -> Reply {
    let body = json!({"jsonrpc": "2.0", "id": "a", "method": method, "params": params});
    send(router, &[("mcp-protocol-version", "2025-11-25")], body).await
}

// ---- modern ------------------------------------------------------------------------------

#[tokio::test]
async fn discover_describes_the_server() {
    let reply = modern(app(Mcp::new()), "server/discover", json!({})).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["id"], 7);
    let result = &reply.body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(
        result["supportedVersions"],
        json!(["2026-07-28", "2025-11-25", "2025-06-18"])
    );
    assert_eq!(
        result["capabilities"],
        json!({"tools": {"listChanged": false}})
    );
    assert_eq!(result["instructions"], "Notes, searchable.");
    assert_eq!(result["ttlMs"], 300_000);
    assert_eq!(result["cacheScope"], "public");
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"],
        json!({"name": "Notes API", "version": "1.2.3"})
    );
    assert!(reply.headers.get("mcp-session-id").is_none());
}

#[tokio::test]
async fn tools_are_listed_from_the_routes() {
    let reply = modern(app(Mcp::new()), "tools/list", json!({})).await;
    let result = &reply.body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["cacheScope"], "public");
    let tools = result["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    // Registration order; `delete_note` has no `mcp` option.
    assert_eq!(names, ["create_note", "search_notes", "get_note", "whoami"]);

    let create = &tools[0];
    assert_eq!(create["title"], "Create a note.");
    assert_eq!(
        create["description"],
        "Create a note.\n\nTitles need not be unique."
    );
    // The body's properties sit at the top level, with their required list.
    let input = &create["inputSchema"];
    assert_eq!(input["type"], "object");
    assert_eq!(
        input["properties"]["title"]["description"],
        "The note's title."
    );
    // `Option<String>` as `anyOf`, not `type: ["string", "null"]`, which some providers reject.
    assert_eq!(
        input["properties"]["body"]["anyOf"],
        json!([{"type": "string"}, {"type": "null"}])
    );
    assert_eq!(input["required"], json!(["title"]));
    assert_eq!(
        create["annotations"],
        json!({"title": "Create a note.", "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false})
    );
    // The success response (201) is the output schema, with its root inlined: legacy clients
    // drop a tool whose output schema has no top-level `"type": "object"`.
    let output = &create["outputSchema"];
    assert_eq!(output["type"], "object");
    assert!(output.get("$ref").is_none());
    assert_eq!(output["properties"]["title"]["type"], "string");

    let search = &tools[1];
    assert_eq!(search["inputSchema"]["required"], json!(["q"]));
    assert_eq!(
        search["inputSchema"]["properties"]["q"]["description"],
        "Words to look for."
    );
    assert_eq!(search["annotations"]["readOnlyHint"], true);
    // An array is a valid output schema in 2026-07-28.
    assert_eq!(search["outputSchema"]["type"], "array");

    let get = &tools[2];
    assert_eq!(get["inputSchema"]["required"], json!(["id"]));
}

#[tokio::test]
async fn a_tool_call_is_a_request_to_the_route() {
    let reply = modern(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "create_note", "arguments": {"title": "hello", "body": "text"}}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let result = &reply.body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(
        result["structuredContent"],
        json!({"id": 1, "title": "hello"})
    );
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(
        result["content"][0]["text"].as_str().unwrap(),
        r#"{"id":1,"title":"hello"}"#
    );
    assert!(result.get("isError").is_none());
}

#[tokio::test]
async fn path_and_query_arguments_fill_the_request() {
    let reply = modern(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "get_note", "arguments": {"id": 1}}),
    )
    .await;
    assert_eq!(reply.body["result"]["structuredContent"]["title"], "first");

    let reply = modern(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "search_notes", "arguments": {"q": "a b&c", "tag": ["x", "y"]}}),
    )
    .await;
    assert_eq!(
        reply.body["result"]["structuredContent"],
        json!(["a b&c", "x", "y"])
    );
}

#[tokio::test]
async fn failures_are_tool_errors_with_the_problem() {
    let reply = modern(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "create_note", "arguments": {"title": ""}}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let result = &reply.body["result"];
    assert_eq!(result["isError"], true);
    assert!(result.get("structuredContent").is_none());
    let problem: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(problem["status"], 422);
    assert_eq!(problem["errors"][0]["pointer"], "/title");

    let reply = modern(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "get_note", "arguments": {"id": 2}}),
    )
    .await;
    let problem: Value =
        serde_json::from_str(reply.body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(reply.body["result"]["isError"], true);
    assert_eq!(problem["detail"], "note 2 not found");
}

#[tokio::test]
async fn a_401_is_the_status_of_the_mcp_response() {
    let reply = modern(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "whoami", "arguments": {}}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.headers[header::WWW_AUTHENTICATE], "Bearer");

    let reply = modern_with(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "whoami", "arguments": {}}),
        &[("authorization", "Bearer s3cret")],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.body["result"]["content"][0]["text"],
        "s3cret via whoami"
    );
}

#[tokio::test]
async fn invalid_calls_are_json_rpc_errors() {
    let reply = modern(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "delete_note", "arguments": {"id": 1}}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["error"]["code"], -32602);
    assert_eq!(reply.body["error"]["message"], "unknown tool: delete_note");

    let reply = modern(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "get_note", "arguments": {}}),
    )
    .await;
    assert_eq!(reply.body["error"]["code"], -32602);
    assert_eq!(
        reply.body["error"]["message"],
        "missing required argument `id`"
    );

    let reply = modern(app(Mcp::new()), "resources/list", json!({})).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body["error"]["code"], -32601);
    assert_eq!(reply.body["id"], 7);
}

#[tokio::test]
async fn modern_headers_must_match_the_body() {
    let reply = modern_with(
        app(Mcp::new()),
        "tools/list",
        json!({}),
        &[("mcp-method", "tools/call")],
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"]["code"], -32020);

    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {
        "_meta": {"io.modelcontextprotocol/protocolVersion": "2099-01-01"}
    }});
    let reply = send(app(Mcp::new()), &[], body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"]["code"], -32022);
    assert_eq!(reply.body["error"]["data"]["requested"], "2099-01-01");
}

// ---- legacy ------------------------------------------------------------------------------

#[tokio::test]
async fn legacy_clients_initialize_without_a_session() {
    let body = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "old", "version": "1"},
    }});
    let reply = send(app(Mcp::new()), &[], body).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.headers.get("mcp-session-id").is_none());
    let result = &reply.body["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "Notes API");
    assert_eq!(
        result["capabilities"]["tools"],
        json!({"listChanged": false})
    );
    assert_eq!(result["instructions"], "Notes, searchable.");
    assert!(result.get("resultType").is_none());

    let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let reply = send(
        app(Mcp::new()),
        &[("mcp-protocol-version", "2025-06-18")],
        notification,
    )
    .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    assert_eq!(reply.body, Value::Null);

    let reply = legacy(app(Mcp::new()), "ping", json!({})).await;
    assert_eq!(reply.body["result"], json!({}));
}

#[tokio::test]
async fn legacy_results_keep_their_shape() {
    let reply = legacy(app(Mcp::new()), "tools/list", json!({})).await;
    let result = &reply.body["result"];
    assert!(result.get("resultType").is_none());
    assert!(result.get("ttlMs").is_none());
    let tools = result["tools"].as_array().unwrap();
    // 2025 revisions only accept object output schemas: the array one is left out.
    assert_eq!(tools[0]["outputSchema"]["type"], "object");
    assert!(tools[1].get("outputSchema").is_none());

    let reply = legacy(
        app(Mcp::new()),
        "tools/call",
        json!({"name": "search_notes", "arguments": {"q": "x"}}),
    )
    .await;
    let result = &reply.body["result"];
    assert!(result.get("structuredContent").is_none());
    assert_eq!(result["content"][0]["text"], r#"["x"]"#);
    assert_eq!(reply.body["id"], "a");

    let reply = legacy(app(Mcp::new()), "server/discover", json!({})).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["error"]["code"], -32601);
}

#[tokio::test]
async fn legacy_can_be_turned_off() {
    let body = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "old", "version": "1"},
    }});
    let reply = send(app(Mcp::new().legacy(false)), &[], body).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"]["code"], -32022);
    assert_eq!(
        reply.body["error"]["data"]["supported"],
        json!(["2026-07-28"])
    );

    let reply = modern(app(Mcp::new().legacy(false)), "server/discover", json!({})).await;
    assert_eq!(
        reply.body["result"]["supportedVersions"],
        json!(["2026-07-28"])
    );
}

// ---- transport ---------------------------------------------------------------------------

#[tokio::test]
async fn only_post_is_served() {
    let response = app(Mcp::new())
        .oneshot(Request::get("/mcp").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()[header::ALLOW], "POST");
}

#[tokio::test]
async fn batches_and_bad_json_are_refused() {
    let reply = send(
        app(Mcp::new()),
        &[],
        json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"]["code"], -32600);
    assert_eq!(reply.body["id"], Value::Null);
}

#[tokio::test]
async fn foreign_origins_are_refused() {
    async fn status(mcp: Mcp, origin: &str, host: &str) -> StatusCode {
        let headers = [("origin", origin), ("host", host)];
        modern_with(app(mcp), "tools/list", json!({}), &headers)
            .await
            .status
    }

    // A local page (the MCP Inspector at localhost:6274) calling a local server.
    assert_eq!(
        status(Mcp::new(), "http://localhost:6274", "127.0.0.1:8000").await,
        StatusCode::OK
    );
    assert_eq!(
        status(Mcp::new(), "https://evil.example", "localhost:8000").await,
        StatusCode::FORBIDDEN
    );
    // DNS rebinding: evil.example now resolves to 127.0.0.1, so its page's origin and the Host
    // it sends agree. Comparing the two would let it in.
    assert_eq!(
        status(Mcp::new(), "http://evil.example:8000", "evil.example:8000").await,
        StatusCode::FORBIDDEN
    );
    // A public origin is refused unless listed, even the endpoint's own.
    assert_eq!(
        status(Mcp::new(), "https://api.example.com", "api.example.com").await,
        StatusCode::FORBIDDEN
    );
    let allowed = Mcp::new().allowed_origins(["https://api.example.com"]);
    assert_eq!(
        status(allowed, "https://api.example.com", "api.example.com").await,
        StatusCode::OK
    );
    // No Origin: not a browser.
    let reply = modern(app(Mcp::new()), "tools/list", json!({})).await;
    assert_eq!(reply.status, StatusCode::OK);
}

#[tokio::test]
async fn nested_apps_and_state_are_served() {
    #[derive(Clone)]
    struct AppState {
        greeting: &'static str,
    }

    /// Say hello.
    #[lesto::get("/hello/{name}", mcp = "tool")]
    async fn hello(State(state): State<AppState>, Path(name): Path<String>) -> String {
        format!("{} {name}", state.greeting)
    }

    let api = App::<AppState>::new().routes(routes![hello]);
    let router = App::<AppState>::new()
        .nest("/api", api)
        .mcp(Mcp::new().path("/agents"))
        .into_router()
        .with_state(AppState { greeting: "hi" });

    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "hello",
        "arguments": {"name": "ann"},
        "_meta": {"io.modelcontextprotocol/protocolVersion": MODERN},
    }});
    let request = Request::post("/agents")
        .header("mcp-protocol-version", MODERN)
        .header("mcp-method", "tools/call")
        .header("mcp-name", "hello")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let reply: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply["result"]["content"][0]["text"], "hi ann");
}

#[tokio::test]
async fn the_endpoint_is_not_documented() {
    let app = App::<()>::new().routes(routes![get_note]).mcp(Mcp::new());
    let spec = app.openapi_json();
    assert!(!spec.contains("/mcp"), "{spec}");
}

#[test]
#[should_panic(expected = "two routes are exposed as the MCP tool `twin`")]
fn explicit_tool_names_must_be_unique() {
    #[lesto::get("/a", mcp(tool, name = "twin"))]
    async fn a() -> &'static str {
        "a"
    }
    #[lesto::get("/b", mcp(tool, name = "twin"))]
    async fn b() -> &'static str {
        "b"
    }
    let _ = App::<()>::new()
        .routes(routes![a, b])
        .mcp(Mcp::new())
        .into_router();
}

#[tokio::test]
async fn colliding_default_names_fall_back_to_the_operation_id() {
    mod one {
        #[lesto::get("/one", mcp = "tool")]
        pub async fn list() -> &'static str {
            "one"
        }
    }
    mod two {
        #[lesto::get("/two", mcp = "tool")]
        pub async fn list() -> &'static str {
            "two"
        }
    }
    let router = App::new()
        .routes(routes![one::list, two::list])
        .mcp(Mcp::new())
        .into_router();
    let reply = modern(router, "tools/list", json!({})).await;
    let names: Vec<&str> = reply.body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["list_one_get", "list_two_get"]);
}

// ---- resources and prompts ---------------------------------------------------------------

/// Every note.
#[lesto::get("/notes", mcp = "resource")]
async fn all_notes() -> (HeaderMap, Json<Vec<Note>>) {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, "public, max-age=30".parse().unwrap());
    let note = Note {
        id: 1,
        title: "first".into(),
    };
    (headers, Json(vec![note]))
}

/// One note.
#[lesto::get("/notes/{id}", responses(404), mcp(resource, name = "note"))]
async fn one_note(Path(id): Path<u64>) -> Result<Json<Note>, HttpError> {
    get_note(Path(id)).await
}

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct Tone {
    /// How the review should sound.
    #[garde(length(min = 1))]
    tone: Option<String>,
}

/// Review a note.
///
/// Asks for a short review of the note's title.
#[lesto::get("/prompts/review/{id}", mcp = "prompt")]
async fn review(Path(id): Path<u64>, Query(tone): Query<Tone>) -> Result<Prompt, HttpError> {
    if id != 1 {
        return Err(HttpError::not_found(format!("note {id} not found")));
    }
    let tone = tone.tone.unwrap_or_else(|| "kind".into());
    Ok(Prompt::new()
        .description(format!("Review of note {id}"))
        .user(format!("Review the note \"first\" in a {tone} tone.")))
}

/// Resources and prompts, next to one tool.
fn library(mcp: Mcp) -> Router {
    App::new()
        .title("Notes API")
        .routes(routes![all_notes, one_note, review, whoami])
        .mcp(mcp)
        .into_router()
}

#[tokio::test]
async fn resources_and_templates_are_listed_from_the_routes() {
    let reply = modern(library(Mcp::new()), "server/discover", json!({})).await;
    assert_eq!(
        reply.body["result"]["capabilities"],
        json!({
            "tools": {"listChanged": false},
            "resources": {"listChanged": false},
            "prompts": {"listChanged": false},
        })
    );

    let reply = modern(library(Mcp::new()), "resources/list", json!({})).await;
    let result = &reply.body["result"];
    assert_eq!(result["ttlMs"], 300_000);
    assert_eq!(
        result["resources"],
        json!([{
            "uri": "lesto://notes-api/notes",
            "name": "all_notes",
            "title": "Every note.",
            "description": "Every note.",
            "mimeType": "application/json",
        }])
    );

    let reply = modern(library(Mcp::new()), "resources/templates/list", json!({})).await;
    assert_eq!(
        reply.body["result"]["resourceTemplates"],
        json!([{
            "uriTemplate": "lesto://notes-api/notes/{id}",
            "name": "note",
            "title": "One note.",
            "description": "One note.",
            "mimeType": "application/json",
        }])
    );

    let reply = legacy(library(Mcp::new()), "resources/list", json!({})).await;
    assert!(reply.body["result"].get("ttlMs").is_none());
    assert_eq!(reply.body["result"]["resources"][0]["name"], "all_notes");
}

async fn read(router: Router, uri: &str) -> Reply {
    let params = json!({"uri": uri});
    modern_with(router, "resources/read", params, &[("mcp-name", uri)]).await
}

#[tokio::test]
async fn reading_a_resource_is_a_get_of_its_path() {
    let reply = read(library(Mcp::new()), "lesto://notes-api/notes/1").await;
    assert_eq!(reply.status, StatusCode::OK);
    let result = &reply.body["result"];
    let contents = &result["contents"][0];
    assert_eq!(contents["uri"], "lesto://notes-api/notes/1");
    assert_eq!(contents["mimeType"], "application/json");
    let note: Value = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
    assert_eq!(note, json!({"id": 1, "title": "first"}));
    // No `Cache-Control`: nothing may be cached.
    assert_eq!(result["ttlMs"], 0);
    assert_eq!(result["cacheScope"], "private");

    // The route's `Cache-Control` becomes the caching fields.
    let reply = read(library(Mcp::new()), "lesto://notes-api/notes").await;
    assert_eq!(reply.body["result"]["ttlMs"], 30_000);
    assert_eq!(reply.body["result"]["cacheScope"], "public");
}

#[tokio::test]
async fn unknown_resources_are_not_found() {
    // The route answers 404, no route matches, another scheme.
    for uri in [
        "lesto://notes-api/notes/2",
        "lesto://notes-api/elsewhere",
        "https://example.com/notes/1",
    ] {
        let reply = read(library(Mcp::new()), uri).await;
        assert_eq!(reply.body["error"]["code"], -32602, "{uri}");
        assert_eq!(reply.body["error"]["data"]["uri"], uri);
    }

    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "resources/read",
        "params": {"uri": "lesto://notes-api/notes/2"}});
    let reply = send(
        library(Mcp::new()),
        &[("mcp-protocol-version", "2025-11-25")],
        body,
    )
    .await;
    assert_eq!(reply.body["error"]["code"], -32002);
}

#[tokio::test]
async fn resource_base_changes_the_uris() {
    let mcp = Mcp::new().resource_base("notes://");
    let reply = modern(library(mcp.clone()), "resources/list", json!({})).await;
    assert_eq!(
        reply.body["result"]["resources"][0]["uri"],
        "notes:///notes"
    );
    let reply = read(library(mcp), "notes:///notes/1").await;
    assert_eq!(
        reply.body["result"]["contents"][0]["uri"],
        "notes:///notes/1"
    );
}

#[tokio::test]
async fn prompts_are_listed_with_their_parameters() {
    let reply = modern(library(Mcp::new()), "prompts/list", json!({})).await;
    assert_eq!(
        reply.body["result"]["prompts"],
        json!([{
            "name": "review",
            "title": "Review a note.",
            "description": "Review a note.\n\nAsks for a short review of the note's title.",
            "arguments": [
                {"name": "id", "required": true},
                {"name": "tone", "description": "How the review should sound.", "required": false},
            ],
        }])
    );
}

#[tokio::test]
async fn getting_a_prompt_runs_the_route() {
    let params = json!({"name": "review", "arguments": {"id": "1", "tone": "blunt"}});
    let reply = modern(library(Mcp::new()), "prompts/get", params).await;
    let result = &reply.body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["description"], "Review of note 1");
    assert_eq!(
        result["messages"],
        json!([{"role": "user", "content": {"type": "text", "text": "Review the note \"first\" in a blunt tone."}}])
    );

    // A 4xx from the route is the arguments' fault: invalid params, with the problem.
    let params = json!({"name": "review", "arguments": {"id": "2"}});
    let reply = modern(library(Mcp::new()), "prompts/get", params).await;
    assert_eq!(reply.body["error"]["code"], -32602);
    assert_eq!(reply.body["error"]["message"], "note 2 not found");
    assert_eq!(reply.body["error"]["data"]["status"], 404);

    let params = json!({"name": "review", "arguments": {"id": "one"}});
    let reply = modern(library(Mcp::new()), "prompts/get", params).await;
    assert_eq!(reply.body["error"]["code"], -32602);
    assert_eq!(reply.body["error"]["data"]["status"], 422, "{}", reply.body);

    let params = json!({"name": "nope"});
    let reply = modern(library(Mcp::new()), "prompts/get", params).await;
    assert_eq!(reply.body["error"]["message"], "unknown prompt: nope");
}

#[tokio::test]
async fn a_prompt_is_json_over_http() {
    let request = Request::get("/prompts/review/1")
        .body(Body::empty())
        .unwrap();
    let response = library(Mcp::new()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let prompt: Prompt = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(prompt.messages().len(), 1);
}

/// Needs a token.
#[lesto::get("/secret", mcp = "resource")]
async fn secret(auth: Bearer) -> String {
    auth.token().to_string()
}

#[tokio::test]
async fn a_resource_behind_auth_answers_401() {
    let router = App::new()
        .title("Notes API")
        .routes(routes![secret])
        .mcp(Mcp::new())
        .into_router();
    let reply = read(router.clone(), "lesto://notes-api/secret").await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert!(reply.headers.contains_key(header::WWW_AUTHENTICATE));

    let params = json!({"uri": "lesto://notes-api/secret"});
    let reply = modern_with(
        router,
        "resources/read",
        params,
        &[
            ("mcp-name", "lesto://notes-api/secret"),
            ("authorization", "Bearer abc"),
        ],
    )
    .await;
    assert_eq!(reply.body["result"]["contents"][0]["text"], "abc");
    assert_eq!(
        reply.body["result"]["contents"][0]["mimeType"],
        "text/plain"
    );
}

#[test]
#[should_panic(expected = "its query parameter `q` is required")]
fn a_resource_cannot_require_a_query_parameter() {
    #[derive(Deserialize, JsonSchema, Validate)]
    struct Q {
        #[garde(skip)]
        q: String,
    }
    /// Search.
    #[lesto::get("/find", mcp = "resource")]
    async fn find(Query(q): Query<Q>) -> String {
        q.q
    }
    let _: Router = App::new()
        .routes(routes![find])
        .mcp(Mcp::new())
        .into_router();
}

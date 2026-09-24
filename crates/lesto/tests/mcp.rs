//! `lesto::mcp` end to end through `oneshot`: both protocol eras, the tool catalog, tool calls
//! as requests to the app's own routes, errors, authentication challenges, Origin checks.

use http_body_util::BodyExt;
use lesto::axum::Router;
use lesto::axum::body::Body;
use lesto::http::{HeaderMap, Request, StatusCode, header};
use lesto::mcp::{Mcp, McpCall};
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
    let evil = [
        ("origin", "https://evil.example"),
        ("host", "localhost:8000"),
    ];
    let reply = modern_with(app(Mcp::new()), "tools/list", json!({}), &evil).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);

    let own = [
        ("origin", "http://localhost:8000"),
        ("host", "localhost:8000"),
    ];
    let reply = modern_with(app(Mcp::new()), "tools/list", json!({}), &own).await;
    assert_eq!(reply.status, StatusCode::OK);

    let allowed = Mcp::new().allowed_origins(["https://evil.example"]);
    let reply = modern_with(app(allowed), "tools/list", json!({}), &evil).await;
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

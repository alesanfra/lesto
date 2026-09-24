// Chapter 16: MCP, operations as tools, resources and prompts for agents (feature `mcp`)
use lesto::http::{HeaderMap, header};
use lesto::mcp::{Mcp, McpCall, Prompt};
use lesto::prelude::*;

#[lesto::model]
pub struct NoteCreate {
    /// What the note is about.
    #[garde(length(min = 1, max = 200))]
    title: String,
    /// Labels to find the note by.
    #[garde(skip)]
    #[serde(default)]
    tags: Vec<String>,
}

#[lesto::model]
pub struct Note {
    id: u64,
    title: String,
    tags: Vec<String>,
}

#[lesto::model]
pub struct Search {
    /// A word the title must contain.
    #[garde(length(min = 1))]
    q: String,
}

/// Create a note.
///
/// Tags are free text; the same tag can be used on many notes.
#[lesto::post("/notes", status = 201, tag = "notes", mcp = "tool")]
async fn create_note(Json(new): Json<NoteCreate>) -> Json<Note> {
    Json(Note {
        id: 1,
        title: new.title,
        tags: new.tags,
    })
}

/// Fetch a note by id.
#[lesto::get("/notes/{id}", tag = "notes", responses(404), mcp = "tool")]
async fn get_note(
    Path(id): Path<u64>,
    call: Option<Extension<McpCall>>,
) -> Result<Json<Note>, HttpError> {
    if let Some(Extension(call)) = call {
        lesto::tracing::info!(tool = call.name(), "called by an agent");
    }
    if id != 1 {
        return Err(HttpError::not_found(format!("note {id} not found")));
    }
    Ok(Json(Note {
        id,
        title: "Groceries".into(),
        tags: vec!["home".into()],
    }))
}

/// Search notes by title.
#[lesto::get("/notes/search", tag = "notes", mcp(tool, name = "search_notes"))]
async fn search(Query(search): Query<Search>) -> Json<Vec<Note>> {
    let _ = search.q;
    Json(Vec::new())
}

/// Delete a note: not exposed to agents.
#[lesto::delete("/notes/{id}", status = 204, tag = "notes")]
async fn delete_note(Path(_id): Path<u64>) {}

/// The text of a note, as Markdown.
#[lesto::get("/notes/{id}/text", tag = "notes", mcp(resource, name = "note_text"))]
async fn note_text(Path(id): Path<u64>) -> Result<(HeaderMap, String), HttpError> {
    if id != 1 {
        return Err(HttpError::not_found(format!("note {id} not found")));
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/markdown".parse().unwrap());
    headers.insert(
        header::CACHE_CONTROL,
        "private, max-age=60".parse().unwrap(),
    );
    Ok((headers, "# Groceries\n\n- milk".into()))
}

#[lesto::model]
pub struct Plan {
    /// When the plan is for, e.g. `this week`.
    #[garde(length(min = 1))]
    when: Option<String>,
}

/// Plan the tasks of a note.
#[lesto::get("/prompts/plan/{id}", tag = "prompts", mcp = "prompt")]
async fn plan(Path(id): Path<u64>, Query(plan): Query<Plan>) -> Result<Prompt, HttpError> {
    if id != 1 {
        return Err(HttpError::not_found(format!("note {id} not found")));
    }
    let when = plan.when.unwrap_or_else(|| "today".into());
    Ok(Prompt::new()
        .user(format!(
            "Turn this note into a plan for {when}:\n\n# Groceries\n\n- milk"
        ))
        .assistant("Here is a plan, one step per line:"))
}

/// The chapter's app: six routes; three tools, a resource, a prompt.
#[allow(dead_code)]
pub fn mcp_app() -> App {
    App::new()
        .title("Notes")
        .routes(routes![
            create_note,
            get_note,
            search,
            delete_note,
            note_text,
            plan
        ])
        .mcp(Mcp::new().instructions("Notes of one user. Search before creating a duplicate."))
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use lesto::axum::body::Body;
    use lesto::http::{Request, StatusCode};
    use lesto::serde_json::{self, Value, json};
    use tower::ServiceExt;

    use super::*;

    /// A `tools/call` in MCP 2026-07-28: the version in `_meta`, method and tool name mirrored
    /// in headers.
    fn call_tool(name: &str, arguments: Value) -> Request<Body> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
                "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"},
            },
        });
        Request::post("/mcp")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", "tools/call")
            .header("mcp-name", name)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Any other 2026-07-28 request; `name` goes in `Mcp-Name` (a prompt's name, a resource's
    /// URI).
    fn request(method: &str, params: Value, name: Option<&str>) -> Request<Body> {
        let mut params = params;
        params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28"});
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut request = Request::post("/mcp")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", method);
        if let Some(name) = name {
            request = request.header("mcp-name", name);
        }
        request.body(Body::from(body.to_string())).unwrap()
    }

    async fn result(request: Request<Body>) -> Value {
        let response = mcp_app().into_router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let reply: Value = serde_json::from_slice(&bytes).unwrap();
        reply["result"].clone()
    }

    #[lesto::test]
    async fn an_agent_creates_a_note() {
        let result = result(call_tool(
            "create_note",
            json!({"title": "Call Ann", "tags": ["work"]}),
        ))
        .await;
        assert_eq!(result["structuredContent"]["title"], "Call Ann");
        assert!(result.get("isError").is_none());
    }

    #[lesto::test]
    async fn validation_failures_reach_the_agent() {
        let result = result(call_tool("create_note", json!({"title": ""}))).await;
        assert_eq!(result["isError"], true);
        let problem: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(problem["status"], 422);
        assert_eq!(problem["errors"][0]["pointer"], "/title");
    }

    #[lesto::test]
    async fn only_marked_routes_are_tools() {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}},
        });
        let request = Request::post("/mcp")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", "tools/list")
            .body(Body::from(body.to_string()))
            .unwrap();
        let result = result(request).await;
        let names: Vec<&str> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["create_note", "get_note", "search_notes"]);
    }

    #[lesto::test]
    async fn a_resource_is_read_by_its_uri() {
        let uri = "lesto://notes/notes/1/text";
        let result = result(request("resources/read", json!({"uri": uri}), Some(uri))).await;
        let contents = &result["contents"][0];
        assert_eq!(contents["mimeType"], "text/markdown");
        assert_eq!(contents["text"], "# Groceries\n\n- milk");
        assert_eq!(result["ttlMs"], 60_000);
        assert_eq!(result["cacheScope"], "private");
    }

    #[lesto::test]
    async fn a_prompt_is_built_by_its_route() {
        let params = json!({"name": "plan", "arguments": {"id": "1", "when": "this week"}});
        let result = result(request("prompts/get", params, Some("plan"))).await;
        assert_eq!(result["messages"][0]["role"], "user");
        let text = result["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(text.starts_with("Turn this note into a plan for this week"));
        assert_eq!(result["messages"][1]["role"], "assistant");
    }
}

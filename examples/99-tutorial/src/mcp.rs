// Chapter 16: MCP, operations as tools for agents (feature `mcp`)
use lesto::mcp::{Mcp, McpCall};
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

/// The chapter's app: four routes, three of them tools.
#[allow(dead_code)]
pub fn mcp_app() -> App {
    App::new()
        .title("Notes")
        .routes(routes![create_note, get_note, search, delete_note])
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
}

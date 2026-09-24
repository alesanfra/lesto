//! lesto's MCP endpoint driven by the official Rust SDK's client (`rmcp`, a dev-dependency), once
//! in each protocol era: what an agent actually sends, over a real socket on loopback.

use lesto::mcp::Mcp;
use lesto::prelude::*;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ProtocolVersion};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::json;

#[derive(Debug, Deserialize, JsonSchema, Validate)]
struct NoteCreate {
    #[garde(length(min = 1))]
    title: String,
}

#[derive(Debug, Serialize, JsonSchema)]
struct Note {
    id: u64,
    title: String,
}

/// Create a note.
#[lesto::post("/notes", status = 201, mcp = "tool")]
async fn create_note(Json(new): Json<NoteCreate>) -> Json<Note> {
    Json(Note {
        id: 1,
        title: new.title,
    })
}

/// Fetch a note.
#[lesto::get("/notes/{id}", responses(404), mcp = "tool")]
async fn get_note(Path(id): Path<u64>) -> Result<Json<Note>, HttpError> {
    Err(HttpError::not_found(format!("note {id} not found")))
}

/// Serve the app on an ephemeral port; the server stops when the returned sender is dropped.
async fn serve() -> (String, tokio::sync::oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let app = App::new()
        .title("Notes")
        .routes(routes![create_note, get_note])
        .mcp(Mcp::new());
    tokio::spawn(app.serve_until(listener, async {
        let _ = stopped.await;
    }));
    (url, stop)
}

async fn exercise(client: RunningService<RoleClient, ()>) {
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, ["create_note", "get_note"]);
    assert!(tools[0].output_schema.is_some());

    let call = |arguments: serde_json::Value, name: &str| -> CallToolRequestParams {
        serde_json::from_value(json!({"name": name, "arguments": arguments})).unwrap()
    };
    let created = client
        .call_tool(call(json!({"title": "hello"}), "create_note"))
        .await
        .unwrap();
    assert_ne!(created.is_error, Some(true));
    assert_eq!(
        created.structured_content,
        Some(json!({"id": 1, "title": "hello"}))
    );

    let invalid = client
        .call_tool(call(json!({"title": ""}), "create_note"))
        .await
        .unwrap();
    assert_eq!(invalid.is_error, Some(true));
    let text = &invalid.content[0].as_text().unwrap().text;
    assert!(text.contains("422"), "{text}");

    let missing = client
        .call_tool(call(json!({"id": 9}), "get_note"))
        .await
        .unwrap();
    assert_eq!(missing.is_error, Some(true));

    client.cancel().await.unwrap();
}

#[tokio::test]
async fn modern_client() {
    let (url, _stop) = serve().await;
    let transport = StreamableHttpClientTransport::from_uri(url);
    let client = ()
        .serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap();
    let info = client.peer_info().unwrap();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
    exercise(client).await;
}

#[tokio::test]
async fn legacy_client() {
    let (url, _stop) = serve().await;
    let transport = StreamableHttpClientTransport::from_uri(url);
    let client = ().serve(transport).await.unwrap();
    let info = client.peer_info().unwrap();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
    assert_eq!(info.server_info.as_ref().unwrap().name, "Notes");
    exercise(client).await;
}

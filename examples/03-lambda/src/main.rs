//! lesto on AWS Lambda. One binary, two lives:
//!
//! ```sh
//! lesto dev -p lambda --port 8765                 # local: binds the port
//! cargo lambda build --release --arm64 -p lambda  # Lambda: `lesto::lambda::serve` starts the runtime
//! ```
//!
//! Tutorial chapter 14 walks through the deployment.

use std::sync::{Arc, Mutex};

use lesto::prelude::*;

#[derive(Clone, Default)]
struct Notes(Arc<Mutex<Vec<Note>>>);

#[lesto::views(Create(text))]
#[derive(Clone, Serialize, Deserialize, JsonSchema, Validate)]
struct Note {
    #[garde(skip)]
    id: u64,
    /// The note itself.
    #[garde(length(min = 1, max = 280))]
    text: String,
}

/// List notes.
#[lesto::get("/notes")]
async fn list_notes(State(notes): State<Notes>) -> Json<Vec<Note>> {
    Json(notes.0.lock().unwrap().clone())
}

/// Read one note.
#[lesto::get("/notes/{id}", responses(404))]
async fn get_note(
    State(notes): State<Notes>,
    Path(id): Path<u64>,
) -> Result<Json<Note>, HttpError> {
    notes
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|n| n.id == id)
        .cloned()
        .map(Json)
        .ok_or_else(|| HttpError::not_found(format!("note {id} not found")))
}

/// Create a note.
#[lesto::post("/notes", status = 201)]
async fn create_note(State(notes): State<Notes>, Json(body): Json<NoteCreate>) -> Json<Note> {
    let mut notes = notes.0.lock().unwrap();
    let mut note = Note {
        id: notes.len() as u64 + 1,
        text: String::new(),
    };
    note.apply_create(body);
    notes.push(note.clone());
    Json(note)
}

fn build_app() -> App<Notes> {
    App::<Notes>::new()
        .title("Notes on Lambda")
        .version("0.1.0")
        .routes(routes![list_notes, get_note, create_note])
}

#[lesto::main]
async fn main() -> Result<(), lesto::lambda::Error> {
    let app = build_app().with_state(Notes::default());
    // In Lambda this runs the function handler; elsewhere it binds LESTO_HOST:LESTO_PORT (or
    // the socket handed over by `lesto dev`).
    lesto::lambda::serve(app).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use lesto::http::StatusCode;
    use serde_json::json;

    /// An HTTP API (payload 2.0) event, as API Gateway would send it.
    fn event(method: &str, path: &str, body: Option<serde_json::Value>) -> String {
        json!({
            "version": "2.0", "routeKey": "$default", "rawPath": path, "rawQueryString": "",
            "headers": {"host": "abc.execute-api.eu-west-1.amazonaws.com", "content-type": "application/json"},
            "requestContext": {
                "accountId": "1", "apiId": "abc", "domainName": "abc.execute-api.eu-west-1.amazonaws.com",
                "domainPrefix": "abc", "requestId": "id", "routeKey": "$default", "stage": "$default",
                "time": "12/Sep/2026:10:00:00 +0000", "timeEpoch": 1789000000000u64,
                "http": {"method": method, "path": path, "protocol": "HTTP/1.1", "sourceIp": "203.0.113.1", "userAgent": "curl"}
            },
            "body": body.map(|b| b.to_string()), "isBase64Encoded": false
        })
        .to_string()
    }

    #[lesto::test]
    async fn create_and_read_through_api_gateway_events() {
        let router = build_app().with_state(Notes::default()).into_router();

        let res = lesto::lambda::test::invoke(
            &router,
            &event("POST", "/notes", Some(json!({"text": "hi"}))),
        )
        .await
        .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);

        let res = lesto::lambda::test::invoke(&router, &event("GET", "/notes/1", None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let note: serde_json::Value = serde_json::from_slice(res.body()).unwrap();
        assert_eq!(note["text"], "hi");

        let res = lesto::lambda::test::invoke(&router, &event("GET", "/notes/9", None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(res.headers()["content-type"], "application/problem+json");
    }
}

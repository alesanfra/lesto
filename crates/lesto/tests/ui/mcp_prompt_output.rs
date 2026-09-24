// A prompt's route returns `lesto::mcp::Prompt`, not data.
use lesto::prelude::*;

#[lesto::get("/prompts/review", mcp = "prompt")]
async fn review() -> Json<Vec<String>> {
    Json(vec![])
}

fn main() {}

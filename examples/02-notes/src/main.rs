//! Notes API on SQLite. Anyone can read; `alice-token` and `bob-token` can write, only alice can
//! delete. Tutorial chapter 13 explains the pieces; the code lives in `src/`.
//!
//! ```sh
//! LESTO_PORT=8765 cargo run -p notes      # or: lesto dev -p notes --port 8765
//! curl -s localhost:8765/notes
//! curl -s -X POST localhost:8765/notes -H 'Authorization: Bearer bob-token' \
//!      -H 'Content-Type: application/json' -d '{"text":"hello"}'
//! ```
//!
//! The same API is an MCP server at `/mcp` (every route but `DELETE`), e.g. for Claude Code:
//!
//! ```sh
//! claude mcp add --transport http notes http://localhost:8765/mcp \
//!     --header 'Authorization: Bearer bob-token'
//! npx @modelcontextprotocol/inspector@latest --cli http://localhost:8765/mcp \
//!     --header 'Authorization: Bearer bob-token' --method tools/call \
//!     --tool-name create_note --tool-arg text=hello
//! ```

use notes::{AppState, Tokens, build_app, connect};

#[lesto::main]
async fn main() -> std::io::Result<()> {
    let state = AppState {
        db: lesto::db::Db::new(connect().await),
        tokens: Tokens::demo(),
    };
    let (host, port) = lesto::bind_address()?;
    println!("http://{host}:{port}/docs");
    build_app().with_state(state).serve().await
}

// Only tools can be exposed over MCP so far.
#[lesto::get("/notes", mcp = "resource")]
async fn notes() -> &'static str {
    "notes"
}

fn main() {}

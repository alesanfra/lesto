// MCP tool names are letters, digits, `_`, `-` and `.`.
#[lesto::get("/notes", mcp(tool, name = "list notes"))]
async fn notes() -> &'static str {
    "notes"
}

fn main() {}

// Resources are read: only GET routes can be one.
#[lesto::post("/notes", mcp = "resource")]
async fn notes() -> &'static str {
    "notes"
}

fn main() {}

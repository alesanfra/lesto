// A view may only list fields that exist on the model.
#[lesto::views(Create(author, body))]
#[derive(serde::Deserialize, schemars::JsonSchema, garde::Validate)]
#[garde(allow_unvalidated)]
struct Note {
    id: u64,
    author: String,
    text: String,
}

fn main() {}

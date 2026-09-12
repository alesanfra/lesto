use lesto::db::prelude::*;
use sqlx::Sqlite;

// `#[store(..)]` takes `read` and `write`, and nothing else.
#[derive(lesto::db::Store)]
#[store(read = "notes:read", permission = "notes:write")]
struct NoteStore<M, P>(Store<M, P, Sqlite>);

fn main() {}

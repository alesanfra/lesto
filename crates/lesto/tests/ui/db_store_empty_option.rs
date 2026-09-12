use lesto::db::prelude::*;
use sqlx::Sqlite;

// An empty `#[store(..)]` declares nothing, which is never what was meant.
#[derive(lesto::db::Store)]
#[store()]
struct NoteStore<M, P>(Store<M, P, Sqlite>);

fn main() {}

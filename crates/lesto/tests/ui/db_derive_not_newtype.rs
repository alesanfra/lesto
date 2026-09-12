use lesto::db::prelude::*;
use sqlx::Sqlite;

// The derive only works on a one-field tuple struct wrapping the inner store.
#[derive(lesto::db::Store)]
struct NoteStore<M, P> {
    inner: Store<M, P, Sqlite>,
}

fn main() {}

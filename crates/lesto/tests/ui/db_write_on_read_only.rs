use lesto::db::prelude::*;
use sqlx::Sqlite;

#[derive(lesto::db::Store)]
struct NoteStore<M, P>(Store<M, P, Sqlite>);

// `write` is only available on `ReadWrite` stores.
impl<P> NoteStore<ReadOnly, P> {
    async fn wipe(&self) -> Result<(), Error> {
        self.write(Anyone, async |conn| {
            sqlx::query("DELETE FROM notes").execute(conn).await?;
            Ok::<_, sqlx::Error>(())
        })
        .await
    }
}

fn main() {}

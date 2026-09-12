use lesto::db::prelude::*;
use sqlx::Sqlite;

#[derive(lesto::db::Store)]
struct NoteStore<M, P>(Store<M, P, Sqlite>);

// A permission string needs an authenticated principal; `Public` never has permissions.
impl<M: Writable> NoteStore<M, Public> {
    async fn create(&self) -> Result<(), Error> {
        self.write("notes:write", async |conn| {
            sqlx::query("INSERT INTO notes DEFAULT VALUES").execute(conn).await?;
            Ok::<_, sqlx::Error>(())
        })
        .await
    }
}

fn main() {}

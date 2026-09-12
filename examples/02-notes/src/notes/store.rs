//! Database access. Every method runs in its own transaction; `read`/`write` check the
//! requirement first, so a caller without the permission never reaches the database.

use lesto::db::prelude::*;
use sqlx::Sqlite;

use super::model::{Note, NoteCreate, NoteUpdate};
use crate::auth::User;

#[derive(lesto::db::Store)]
pub struct NoteStore<M, P>(Store<M, P, Sqlite>);

/// Reads: any mode, any principal.
impl<M: Mode, P> NoteStore<M, P> {
    pub async fn list(&self) -> Result<Vec<Note>, Error> {
        self.read(Anyone, async |conn| {
            sqlx::query_as("SELECT id, author, text FROM notes ORDER BY id")
                .fetch_all(conn)
                .await
        })
        .await
    }

    pub async fn get(&self, id: i64) -> Result<Note, Error> {
        self.read(Anyone, async |conn| {
            sqlx::query_as("SELECT id, author, text FROM notes WHERE id = ?")
                .bind(id)
                .fetch_one(conn) // RowNotFound → 404
                .await
        })
        .await
    }
}

/// Writes: read-write stores held by a `User`.
impl<M: Writable> NoteStore<M, User> {
    pub async fn create(&self, body: NoteCreate) -> Result<Note, Error> {
        let author = self.principal().name.clone();
        self.write("notes:write", async |conn| {
            sqlx::query_as(
                "INSERT INTO notes (author, text) VALUES (?, ?) RETURNING id, author, text",
            )
            .bind(&author)
            .bind(&body.text)
            .fetch_one(conn) // UNIQUE violation → 409
            .await
        })
        .await
    }

    /// Only the author may change a note; the view carries the changed fields.
    pub async fn update(&self, id: i64, body: NoteUpdate) -> Result<Note, Error> {
        let me = self.principal().name.clone();
        self.write("notes:write", async |conn| {
            let mut note: Note = sqlx::query_as("SELECT id, author, text FROM notes WHERE id = ?")
                .bind(id)
                .fetch_one(&mut *conn)
                .await?;
            if note.author != me {
                return Err(Error::http(lesto::HttpError::forbidden(
                    "Only the author can edit a note",
                )));
            }
            note.apply_update(body);
            sqlx::query("UPDATE notes SET text = ? WHERE id = ?")
                .bind(&note.text)
                .bind(id)
                .execute(conn)
                .await?;
            Ok(note)
        })
        .await
    }

    pub async fn delete(&self, id: i64) -> Result<(), Error> {
        self.write("notes:delete", async |conn| {
            let result = sqlx::query("DELETE FROM notes WHERE id = ?")
                .bind(id)
                .execute(conn)
                .await?;
            if result.rows_affected() == 0 {
                return Err(Error::not_found("No such note"));
            }
            Ok(())
        })
        .await
    }
}

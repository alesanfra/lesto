//! The `Note` model and its request views.

use lesto::prelude::*;

/// A note. `#[lesto::views]` generates `NoteCreate { text }` for POST and
/// `NoteUpdate { text: Option<String> }` for PATCH, with the same validation rules.
#[lesto::views(Create(text), Update(text?))]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Validate, sqlx::FromRow)]
pub struct Note {
    #[garde(skip)]
    pub id: i64,
    /// Who wrote it (from the token).
    #[garde(skip)]
    pub author: String,
    /// The note itself.
    #[garde(length(min = 1, max = 280))]
    pub text: String,
}

//! The `Note` model and its request views.

/// A note. `#[lesto::views]` generates `NoteCreate { text }` for POST and
/// `NoteUpdate { text: Option<String> }` for PATCH, with the same validation rules.
#[lesto::model(views(Create(text), Update(text?)))]
#[derive(Debug, Clone, sqlx::FromRow)]
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

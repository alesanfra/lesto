# 6. Responses

The handler's return type decides what goes on the wire and what appears in the documentation.

## JSON

```rust
#[derive(Serialize, JsonSchema)]
struct User {
    id: u64,
    name: String,
}

/// Current user.
#[lesto::get("/me")]
async fn me() -> Json<User> {
    Json(User { id: 1, name: "Ada".into() })
}
```

`Json<T>` as output serializes `T` with `Content-Type: application/json`. It needs `Serialize` for
the value and `JsonSchema` for the documentation: the `200` response references the `User` schema.

A `Vec<T>` is documented as an array:

```rust
#[lesto::get("/users")]
async fn list() -> Json<Vec<User>> { .. }     // schema: { type: array, items: $ref User }
```

## The status code

By default a successful response is `200`. To change it, use `status` on the attribute:

```rust
#[lesto::post("/users", status = 201)]
async fn create() -> Json<User> { .. }

#[lesto::delete("/users/{id}", status = 204)]
async fn delete(Path(_id): Path<u64>) {}
```

The status applies both at runtime and in the documentation. A handler returning `()` produces an
empty body, and with `status = 204` that is the classic deletion response.

## Text and HTML

```rust
#[lesto::get("/health")]
async fn health() -> &'static str { "ok" }               // text/plain

#[lesto::get("/page")]
async fn page() -> lesto::axum::response::Html<String> { .. }   // text/html
```

## `Result`: success or error

In most handlers something can go wrong. The natural return type is a `Result` with `HttpError`
as the error:

```rust
/// One user by id.
#[lesto::get("/users/{id}", responses(404))]
async fn get_user(Path(id): Path<u64>) -> Result<Json<User>, HttpError> {
    find(id).map(Json).ok_or_else(|| HttpError::not_found(format!("user {id} not found")))
}
```

The documentation shows `200` with `User`, `404` (thanks to `responses(404)`) and a `default`
response for other errors. The next chapter goes deeper into `HttpError`.

With `?` you can propagate errors like in any Rust function:

```rust
async fn handler(Json(body): Json<Input>) -> Result<Json<Output>, HttpError> {
    let user = load_user(body.user_id).await?;   // if load_user returns HttpError
    Ok(Json(process(user)))
}
```

If your service function returns a different error, implement `From<YourError> for HttpError` and
`?` will convert.

## Input model different from output model

Like `response_model` in FastAPI, use two structs: one for the incoming body (no `id`, no computed
fields, with the password) and one for the output (with `id`, without password).

```rust
#[derive(Deserialize, JsonSchema, Validate)]
struct UserIn {
    #[garde(length(min = 1))] name: String,
    #[garde(length(min = 8))] password: String,
}

#[derive(Serialize, JsonSchema)]
struct UserOut {
    id: u64,
    name: String,
}

#[lesto::post("/users", status = 201)]
async fn create_user(Json(input): Json<UserIn>) -> Json<UserOut> {
    let id = save(&input.name, &input.password);
    Json(UserOut { id, name: input.name })
}
```

The password cannot end up in the response: the type does not contain it.

### One model, several views: `views(..)`

When the input and output structs are the *same fields minus a few*, writing them by hand
duplicates every attribute. Declare the model once and let lesto generate the views:

```rust
#[lesto::model(views(Create(author, text), Update(text?)))]
#[derive(Clone)]
struct Note {
    #[garde(skip)]
    id: u64,
    /// Who wrote it.
    #[garde(length(min = 1, max = 64))]
    author: String,
    #[garde(length(min = 1))]
    text: String,
}

/// Create a note.
#[lesto::post("/notes", status = 201)]
async fn create_note(Json(body): Json<NoteCreate>) -> Json<Note> {
    let mut note = Note { id: next_id(), author: String::new(), text: String::new() };
    note.apply_create(body);          // copies author and text
    Json(note)
}

/// Change the text of a note.
#[lesto::patch("/notes/{id}")]
async fn update_note(Path(id): Path<u64>, Json(body): Json<NoteUpdate>) -> Json<Note> {
    let mut note = load(id);
    note.apply_update(body);          // text is Option<String>: copied only when present
    Json(note)
}
```

The attribute generates `NoteCreate { author, text }` and `NoteUpdate { text: Option<String> }`.
Each field is copied with its doc comment and its serde, garde and schemars attributes, so the
views validate and document exactly like the model. The views also inherit the model's derives and
container attributes (`#[serde(rename_all = "camelCase")]`, `#[garde(allow_unvalidated)]`, ...).
With plain derives, the standalone attribute does the same: `#[lesto::views(..)]`, written
**above** `#[derive]` so it can see them. Only what describes a request
payload is copied: the std derives (`Debug`, `Clone`, `PartialEq`, `Default`, ...), `Serialize`,
`Deserialize`, `JsonSchema`, `Validate`, and the `serde`, `garde`, `schemars` and `doc`
attributes. A `#[derive(sqlx::FromRow)]` on the model, and the `#[sqlx(..)]` attributes that go
with it, stay on the model.

A field marked with `?` becomes `Option<T>` with `#[serde(default)]`, and its garde rules are
wrapped in `inner(..)` so they still apply when a value is present (at runtime only: schemars does
not mirror `inner(..)` rules into the schema, so the optional field shows as nullable without
`minLength`). For every view you get a
`Model::apply_<view>(&mut self, view)` method that copies the fields back, skipping absent
optionals. In the documentation the views are separate schemas, `NoteCreate` and `NoteUpdate`,
with their own `required` lists. Naming a field that does not exist is a compile error that lists
the available ones.

## Other supported return types

| Type | Response | Documentation |
|---|---|---|
| `Json<T>` | JSON | schema of `T` |
| `String`, `&'static str` | `text/plain` | string |
| `()` | empty body | status only |
| `Html<T>` | `text/html` | string |
| `Result<T, E>` | `T` or `E` | union of both |
| `(StatusCode, T)` | `T` with the given status | like `T` (status unknown at compile time) |
| `(HeaderMap, T)` | `T` with extra headers | like `T` |
| `StatusCode`, `Response`, `Redirect` | as in axum | none |

You can document a type of your own by implementing `lesto::OperationOutput`.

## Recap

- `Json<T>` for JSON, `String` for text, `()` for empty bodies.
- `status = N` on the attribute changes the success code, at runtime too.
- `Result<T, HttpError>` for handlers that can fail; `responses(404, ...)` to document them.
- Two structs, one in and one out, when the fields differ; `#[lesto::views]` generates them
  from one model.

Next: [Error handling](07-errors.md).

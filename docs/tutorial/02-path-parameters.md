# 2. Path parameters

A path parameter is a variable part of the URL, like the `id` in `/items/42`.

## One parameter

```rust
use lesto::prelude::*;

/// Returns the received id.
#[lesto::get("/items/{item_id}")]
async fn read_item(Path(item_id): Path<u64>) -> String {
    format!("item {item_id}")
}
```

`{item_id}` in the path declares the parameter; `Path<u64>` extracts and converts it.
`Path(item_id)` is a plain Rust pattern that opens the wrapper and hands you the value.

```sh
curl http://127.0.0.1:8000/items/42
```

```
item 42
```

## The type matters

You declared `u64`, so lesto **converts and checks**. Try a value that is not an integer:

```sh
curl http://127.0.0.1:8000/items/foo
```

```json
{
  "type": "about:blank",
  "title": "Unprocessable Entity",
  "status": 422,
  "detail": "1 validation error",
  "instance": "/items/foo",
  "errors": [
    { "in": "path", "pointer": "/item_id", "detail": "Cannot parse `foo` to a `u64`", "code": "value_error" }
  ]
}
```

A `422` response with a standard body (RFC 9457, covered in the errors chapter) that says exactly
which parameter failed and why. Inside the handler you already work with a valid `u64`.

In the documentation the parameter shows as `integer`, `required`, with format `uint64`.

## Several parameters

With two or more parameters you can use a tuple, in the order they appear in the path:

```rust
#[lesto::get("/users/{user_id}/items/{item_id}")]
async fn read_user_item(Path((user_id, item_id)): Path<(u64, u64)>) -> String {
    format!("user {user_id}, item {item_id}")
}
```

or a struct, more readable when there are many. Field names must match the names in the path:

```rust
#[lesto::model]
struct UserItemPath {
    user_id: u64,
    item_id: u64,
}

#[lesto::get("/users/{user_id}/items/{item_id}")]
async fn read_user_item(Path(p): Path<UserItemPath>) -> String {
    format!("user {}, item {}", p.user_id, p.item_id)
}
```

Note the two derives: `Deserialize` (serde) to read the values, `JsonSchema` (schemars) to
document them. You will see them on almost every type that goes through the API.

## Describing a parameter

A doc comment on the field becomes the parameter's description:

```rust
#[lesto::model]
struct UserItemPath {
    /// Id of the owning user.
    user_id: u64,
    /// Id of the item.
    item_id: u64,
}
```

## Enums

If a parameter may only take a few values, use an `enum`: conversion fails for other values and
the documentation shows the list.

```rust
#[lesto::model]
#[serde(rename_all = "lowercase")]
enum ModelName {
    Alexnet,
    Resnet,
    Lenet,
}

#[lesto::get("/models/{name}")]
async fn get_model(Path(name): Path<ModelName>) -> String {
    match name {
        ModelName::Alexnet => "Deep Learning FTW!".into(),
        ModelName::Lenet => "LeCNN all the images".into(),
        ModelName::Resnet => "Have some residuals".into(),
    }
}
```

`#[serde(rename_all = "lowercase")]` makes `/models/alexnet` acceptable. schemars reads serde's
attributes, so the documentation shows lowercase names as well.

## Route order

Routes are matched by structure, not by registration order: `/users/me` and `/users/{id}`
coexist, and `/users/me` wins because it is more specific. You do not need to worry about the
order in `routes![]`.

## Recap

- `{name}` in the path + `Path<T>` in the handler.
- The type `T` does conversion, validation and documentation.
- Tuple or struct for several parameters; enum for closed sets of values.
- A value that cannot be converted yields a detailed `422`, with no code on your side.

Next: [Query parameters](03-query-parameters.md).

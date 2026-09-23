# 4. Request body

When the client has to send data (create, update) it sends it in the request **body**, usually as
JSON. In lesto the body is declared with a struct and extracted with `Json<T>`.

```rust
use lesto::prelude::*;

#[lesto::model]
struct Item {
    /// Item name.
    #[garde(length(min = 1, max = 100))]
    name: String,
    #[garde(skip)]
    description: Option<String>,
    /// Price in euros.
    #[garde(range(min = 0.0))]
    price: f64,
    #[garde(skip)]
    tax: Option<f64>,
}

#[lesto::model]
struct ItemOut {
    name: String,
    price_with_tax: f64,
}

/// Create an item.
#[lesto::post("/items", status = 201)]
async fn create_item(Json(item): Json<Item>) -> Json<ItemOut> {
    let tax = item.tax.unwrap_or(0.0);
    Json(ItemOut {
        name: item.name,
        price_with_tax: item.price + tax,
    })
}
```

```sh
curl -X POST http://127.0.0.1:8000/items \
  -H 'Content-Type: application/json' \
  -d '{"name": "Keyboard", "price": 45.0, "tax": 9.9}'
```

```
HTTP/1.1 201 Created
{"name":"Keyboard","price_with_tax":54.9}
```

## `#[lesto::model]`: where the derives come from

A type that travels as JSON needs four derives: serde's `Serialize` and `Deserialize` (the wire
format), schemars' `JsonSchema` (the OpenAPI schema) and garde's `Validate` (the rules).
`#[lesto::model]` writes them for you, through lesto's own copies of those crates, which is why
your `Cargo.toml` does not list them. The two structs above expand to roughly:

```rust
#[derive(Serialize, Deserialize, JsonSchema, Validate)]
#[serde(crate = "::lesto::serde")]
#[schemars(crate = "::lesto::schemars")]
struct Item { /* the fields, as written */ }

#[derive(Serialize, Deserialize, JsonSchema, Validate)]
#[serde(crate = "::lesto::serde")]
#[schemars(crate = "::lesto::schemars")]
#[garde(allow_unvalidated)]          // ItemOut has no rules at all
struct ItemOut { /* ... */ }
```

Two things to know:

- **Rules are all or nothing.** A model with no `#[garde(..)]` anywhere (`ItemOut`) accepts every
  field as is. As soon as one field has a rule (`Item`), every field needs one: a rule, `skip`,
  or `dive` for a nested model. That is what catches a forgotten `dive`, which would otherwise
  leave a nested struct unchecked.
- **Your other derives stay yours.** Write `#[derive(Debug, Clone)]` below the attribute as usual;
  field attributes (`#[serde(rename = ..)]`, `#[schemars(..)]`, `#[garde(..)]`) work unchanged.

The plain derives still work, if you prefer to spell them out or need one without the others:
then `serde`, `schemars` and `garde` must be dependencies of your crate, as with any derive.

## What `Json<T>` does for you

In order:

1. Checks that `Content-Type` is JSON (`application/json` or `application/*+json`). Otherwise it
   answers `415 Unsupported Media Type`.
2. Reads the body and deserializes it into `T`. Missing fields, wrong types or malformed JSON
   yield a `422` that names the field.
3. Runs garde validation. Every violated rule becomes an entry in `errors`.
4. Hands you a **valid** `T`. Inside the handler there is nothing left to re-check.

In the documentation the operation has a required `requestBody` with the `Item` schema and a
`201` response with the `ItemOut` schema. Both schemas end up in `components.schemas`, named after
the struct.

## A wrong body

Missing field:

```sh
curl -X POST http://127.0.0.1:8000/items -H 'Content-Type: application/json' \
  -d '{"name": "Keyboard"}'
```

```json
{
  "type": "about:blank", "title": "Unprocessable Entity", "status": 422,
  "detail": "1 validation error", "instance": "/items",
  "errors": [
    { "in": "body", "pointer": "/price", "detail": "missing field `price`", "code": "missing" }
  ]
}
```

Violated rule:

```sh
curl -X POST http://127.0.0.1:8000/items -H 'Content-Type: application/json' \
  -d '{"name": "", "price": -1}'
```

```json
{
  "...": "...",
  "detail": "2 validation errors",
  "errors": [
    { "in": "body", "pointer": "/name",  "detail": "length is lower than 1", "code": "value_error" },
    { "in": "body", "pointer": "/price", "detail": "lower than 0",           "code": "value_error" }
  ]
}
```

Invalid JSON:

```json
{ "errors": [ { "in": "body", "pointer": "", "detail": "expected value at line 1 column 2", "code": "json_invalid" } ] }
```

The empty `pointer` means the whole document.

## Body + path + query together

Extractors combine freely. lesto works out where each value comes from.

```rust
#[lesto::put("/items/{item_id}")]
async fn update_item(
    Path(item_id): Path<u64>,
    Query(q): Query<Pagination>,
    Json(item): Json<Item>,
) -> String {
    format!("update {item_id} ({} chars, skip={})", item.name.len(), q.skip)
}
```

One rule, inherited from axum: **the body goes last**, because it consumes the request.

## Optional fields and defaults

The same serde rules you saw for queries apply:

```rust
#[derive(Deserialize, JsonSchema, Validate)]
#[garde(allow_unvalidated)]
struct Item {
    name: String,
    #[serde(default)]
    tags: Vec<String>,          // absent → empty vector
    #[serde(default = "one")]
    quantity: u32,              // absent → 1
    note: Option<String>,       // absent → None
}

fn one() -> u32 { 1 }
```

A field with `#[serde(default)]` or of type `Option` does not appear in the schema's `required`.

## camelCase names

If the client wants `priceWithTax` instead of `price_with_tax`:

```rust
#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ItemOut {
    name: String,
    price_with_tax: f64,
}
```

schemars follows serde: the schema uses `priceWithTax` too.

## Recap

- `Json<T>` with `T: Deserialize + JsonSchema + Validate`.
- Content type, parsing and validation are automatic; the handler receives a valid value.
- Errors say which field (`pointer`) and why (`detail`, `code`).
- Path, query and body combine; the body goes last.

Next: [Validation](05-validation.md).

# 3. Query parameters

Everything after the `?` in the URL: `/items?skip=0&limit=10`.

In lesto, query parameters are declared with a struct and extracted with `Query<T>`.

```rust
use lesto::prelude::*;

#[lesto::model]
struct Pagination {
    /// How many items to skip.
    #[garde(skip)]
    #[serde(default)]
    skip: usize,
    /// How many items to return (1..=100).
    #[garde(range(min = 1, max = 100))]
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

/// List items.
#[lesto::get("/items")]
async fn list_items(Query(q): Query<Pagination>) -> String {
    format!("skip={} limit={}", q.skip, q.limit)
}
```

```sh
curl "http://127.0.0.1:8000/items?skip=20&limit=5"
```

```
skip=20 limit=5
```

```sh
curl "http://127.0.0.1:8000/items"
```

```
skip=0 limit=10
```

## Three derives, three jobs

- `Deserialize` reads the values from the query string. With `#[serde(default)]` a missing field
  takes its default value; with `#[serde(default = "fn")]` you pick one.
- `JsonSchema` produces the documentation: every field becomes an `in: query` parameter, with
  type, default and the description taken from the doc comment.
- `Validate` (garde) adds the rules: here `limit` must be between 1 and 100.

Every field needs a `#[garde(...)]` attribute: a rule, or `#[garde(skip)]` if there is nothing to
check. If you would rather not annotate rule-less fields, put `#[garde(allow_unvalidated)]` on the
struct.

## When validation fails

```sh
curl "http://127.0.0.1:8000/items?limit=500"
```

```json
{
  "type": "about:blank",
  "title": "Unprocessable Entity",
  "status": 422,
  "detail": "1 validation error",
  "instance": "/items",
  "errors": [
    { "in": "query", "pointer": "/limit", "detail": "greater than 100", "code": "value_error" }
  ]
}
```

The same happens with a wrong type (`?limit=abc`): `code` is `value_error` and `detail` explains.

## Optional parameters

An `Option<T>` field is optional and, when absent, is `None`. In the documentation it shows as
`required: false`.

```rust
#[lesto::model]
#[garde(allow_unvalidated)]
struct Search {
    q: Option<String>,
    #[serde(default)]
    exact: bool,
}
```

To validate the content of an `Option` use `inner`: `#[garde(inner(length(min = 3)))]` checks the
string only when it is present.

## Repeated parameters

A key that appears several times (`?tag=rust&tag=web`) collects into a `Vec<T>`. Give the field
`#[serde(default)]` so that a query string without the key is an empty list rather than a `422`.

```rust
#[lesto::model]
struct Tags {
    #[garde(inner(length(min = 1)))]
    #[serde(default)]
    tag: Vec<String>,
}

/// List items with any of the tags.
#[lesto::get("/tags")]
async fn list_tags(Query(t): Query<Tags>) -> String {
    t.tag.join(",")
}
```

```sh
curl "http://127.0.0.1:8000/tags?tag=rust&tag=web"
```

```
rust,web
```

In the documentation the parameter has an array schema; OpenAPI's default for query arrays
(`style: form, explode: true`) is exactly the repeated-key form.

## What ends up in the documentation

For the `Pagination` struct above, the `GET /items` operation has two parameters:

```json
{ "name": "skip",  "in": "query", "required": false, "description": "How many items to skip.",
  "schema": { "type": "integer", "format": "uint", "minimum": 0, "default": 0 } }
{ "name": "limit", "in": "query", "required": false, "description": "How many items to return (1..=100).",
  "schema": { "type": "integer", "format": "uint", "minimum": 1, "maximum": 100, "default": 10 } }
```

Note `minimum: 1` and `maximum: 100`: schemars also reads `#[garde(...)]` attributes, so the
validation rules show up in the schema without being duplicated.

## Recap

- A struct with `Deserialize + JsonSchema + Validate`, extracted with `Query<T>`.
- `#[serde(default)]` for defaults, `Option<T>` for optionals, `Vec<T>` for repeated keys.
- Rules with `#[garde(...)]`, `#[garde(skip)]` where none apply.
- Type and validation errors yield a `422` with a pointer to the field.

Next: [Request body](04-request-body.md).

# 5. Validation

Validation is handled by [garde](https://docs.rs/garde). This chapter collects the most used rules
and the situations you will run into first.

## The rules

Every field of a `#[derive(Validate)]` struct needs exactly one `#[garde(...)]` attribute (or
`skip`). Several rules combine with commas.

| Rule | Applies to | Example |
|---|---|---|
| `length(min = a, max = b)` | strings, `Vec`, maps | `#[garde(length(min = 1, max = 64))]` |
| `byte_length(...)` | strings (bytes instead of chars) | `#[garde(byte_length(max = 255))]` |
| `range(min = a, max = b)` | numbers | `#[garde(range(min = 1, max = 100))]` |
| `email` | strings | `#[garde(email)]` |
| `url` | strings | `#[garde(url)]` |
| `ip`, `ipv4`, `ipv6` | strings | `#[garde(ipv4)]` |
| `alphanumeric`, `ascii` | strings | `#[garde(alphanumeric)]` |
| `contains("x")`, `prefix("x")`, `suffix("x")` | strings | `#[garde(prefix("sk_"))]` |
| `pattern(r"...")` | strings (`regex` feature) | `#[garde(pattern(r"^[a-z]+$"))]` |
| `matches(other)` | equal to another field | `#[garde(matches(password))]` |
| `required` | `Option<T>` must be `Some` | `#[garde(required)]` |
| `inner(...)` | applies the rule to the elements | `#[garde(inner(length(min = 1)))]` |
| `dive` | validates a nested struct | `#[garde(dive)]` |
| `custom(fn)` | your own function | `#[garde(custom(is_even))]` |
| `skip` | no check | `#[garde(skip)]` |

`email` and `url` need the garde features of the same name (`features = ["derive", "email", "url"]`);
`pattern` needs `regex`.

## A complete example

```rust
use lesto::prelude::*;

#[derive(Deserialize, JsonSchema, Validate)]
struct Address {
    #[garde(length(min = 1))]
    street: String,
    #[garde(length(min = 2, max = 2), ascii)]
    country: String,
}

#[derive(Deserialize, JsonSchema, Validate)]
struct SignUp {
    #[garde(length(min = 3, max = 32), alphanumeric)]
    username: String,
    #[garde(email)]
    email: String,
    #[garde(length(min = 8))]
    password: String,
    #[garde(matches(password))]
    password_confirm: String,
    /// Age; optional but, when present, at least 18.
    #[garde(inner(range(min = 18)))]
    age: Option<u8>,
    /// At least one address, each of them valid.
    #[garde(length(min = 1), dive)]
    addresses: Vec<Address>,
    #[garde(custom(no_spaces))]
    display_name: String,
}

fn no_spaces(value: &str, _ctx: &()) -> garde::Result {
    if value.contains(' ') {
        return Err(garde::Error::new("must not contain spaces"));
    }
    Ok(())
}

#[lesto::post("/signup", status = 201)]
async fn signup(Json(body): Json<SignUp>) -> String {
    format!("welcome {}", body.username)
}
```

A body with two addresses, the second one without a street, produces:

```json
{
  "errors": [
    { "in": "body", "pointer": "/addresses/1/street", "detail": "length is lower than 1", "code": "value_error" }
  ]
}
```

The `pointer` walks through structs and arrays: `/addresses/1/street` is the `street` field of the
second address. It is a JSON Pointer (RFC 6901), so a client can map the error onto the right
form field.

## What reaches the documentation

schemars knows garde's attributes and translates them into JSON Schema constraints:

| garde | JSON Schema |
|---|---|
| `length(min, max)` on a string | `minLength`, `maxLength` |
| `length(min, max)` on a `Vec` | `minItems`, `maxItems` |
| `range(min, max)` | `minimum`, `maximum` |
| `email` | `format: email` |
| `url` | `format: uri` |
| `pattern(r)` | `pattern` |
| `required` | the field leaves `Option` and enters `required` |

A `custom` rule has no translation: if you want it documented, say so in the field's doc comment.

## Structs without rules

If a struct has nothing to validate but still has to go through `Json<T>` or `Query<T>`:

```rust
#[derive(Deserialize, JsonSchema, Validate)]
#[garde(allow_unvalidated)]
struct Filters {
    tag: Option<String>,
    archived: bool,
}
```

## Validating outside the extractors

`Validate` is an ordinary trait. You can call it wherever you like:

```rust
let report = body.validate();       // Result<(), garde::Report>
```

and turn the report into an HTTP error with `lesto::ValidationError::from_garde("body", &report)`,
which is exactly what `Json<T>` does.

## Recap

- One `#[garde(...)]` attribute per field, or `allow_unvalidated` on the struct.
- `dive` for nested structs, `inner` for the elements of `Option` and `Vec`.
- `custom(fn)` for your own rules: `fn(&T, &()) -> garde::Result`.
- Constraints end up in the OpenAPI schema as well.

Next: [Responses](06-responses.md).

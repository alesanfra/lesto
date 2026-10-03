# A. From FastAPI to lesto

| FastAPI | lesto |
|---|---|
| `app = FastAPI(title="X", version="1.0")` | `App::new().title("X").version("1.0")` |
| `@app.get("/items/{id}")` | `#[lesto::get("/items/{id}")]` |
| `status_code=201` | `status = 201` |
| `tags=["items"]` | `tag = "items"` / `tags("a", "b")` |
| `summary=`, `description=` | doc comment `///` (or `summary = "..."`) |
| `deprecated=True` | `deprecated` |
| `responses={404: {...}}` | `responses(404)` |
| `operation_id=` | `operation_id = "..."` (default: `{function}_{path}_{method}`, as in FastAPI) |
| `item_id: int` (path) | `Path(item_id): Path<u64>` |
| `q: str \| None = None` (query) | struct with `q: Option<String>` in `Query<T>` |
| `limit: int = Query(10, ge=1, le=100)` | `#[serde(default = ...)] #[garde(range(min = 1, max = 100))]` |
| `item: Item` (body, pydantic) | `Json(item): Json<Item>` (serde + garde) |
| `class Item(BaseModel)` | `#[derive(Deserialize, JsonSchema, Validate)] struct Item` |
| `Field(min_length=1)` | `#[garde(length(min = 1))]` |
| `EmailStr` | `String` + `#[garde(email)]` |
| `response_model=Out` | return type `Json<Out>` |
| `class NoteCreate(NoteBase)`, `class Note(NoteBase)` | `#[lesto::views(Create(author, text), Update(text?))]` on the model |
| `raise HTTPException(404, "...")` | `Err(HttpError::not_found("..."))` |
| `Depends(get_db)` | `State<Db>`, or a `lesto::db` store: `NoteStore<ReadOnly, Public>` (chapter 13) |
| `Depends(get_current_user)` | custom extractor (`FromRequestParts`), or `impl Authenticated for User` + `NoteStore<_, User>` (chapter 13) |
| `Depends(require_scope("notes:write"))` | `self.write("notes:write", ..)` in the store method (chapter 13) |
| `db.begin()` / `db.commit()` | implicit: one transaction per store method |
| `Depends(OAuth2PasswordBearer(...))` | `Bearer` / `Bearer<MyOAuth2>` |
| `Security(APIKeyHeader(name="X-Key"))` | `ApiKey<MyKey>` |
| `Depends(HTTPBasic())` | `Basic` |
| `jwt.decode(token, jwks_key, audience=..., issuer=...)` in a dependency | `Jwt<Claims>` with `App::oidc(Oidc::discover(url).audiences([..]).await?)` (feature `oidc`, chapter 9) |
| `Security(dep, scopes=["read"])` | `AuthScheme::scopes()` on the marker |
| `APIRouter(prefix="/users")` + `include_router` | `App::new().routes(...)` + `nest("/users", app)` |
| `app.add_middleware(CORSMiddleware, ...)` | `.layer(CorsLayer::...)` |
| `@app.middleware("http")` | `.layer(middleware::from_fn(f))` |
| `TestClient(app)` | `app.into_router().oneshot(req)` |
| `/docs`, `/redoc`, `/openapi.json` | `/docs` (Scalar), `/swagger` (Swagger UI), `/openapi.json` |
| `docs_url=None` | `.docs_url(None)` |
| `uvicorn.run(app, host="0.0.0.0", port=8000)` | `.serve().await` with `LESTO_HOST=0.0.0.0 LESTO_PORT=8000` (or `.serve_at("0.0.0.0:8000")`) |
| `{"detail": ...}` errors | RFC 9457 `application/problem+json` |

## Attribute options

Every option of the route attributes (`get`, `post`, `put`, `patch`, `delete`, `head`,
`options`):

```text
#[lesto::get("/path", status = 200, tag = "x", tags("a", "b"), summary = "...",
             description = "...", operation_id = "...", deprecated, responses(404, 409),
             security("bearerAuth"), public, state = AppState,
             mcp = "tool" | "resource" | "prompt")]
```

- `status = N` rewrites every `200` of the handler; to put the status in the type instead, return
  `Created<T>` (201), `Accepted<T>` (202) or `NoContent` (204), documented under that status.
- `state = T` only affects the compile-time checks the macro emits; otherwise the state comes
  from a `State<T>` argument or from the `App<S>` the `routes![]` set is added to (see
  [Common problems](B-common-problems.md)).
- The default `operation_id` is `{function}_{path}_{method}` (`get_user_users__id__get`), unique
  per route as in FastAPI.
- `mcp = "tool"` (feature `mcp`) exposes the operation as an MCP tool named after the function;
  `mcp(tool, name = "search_notes")` names it. `mcp = "resource"` (a `GET` route) serves it as a
  resource at `lesto://{title}{path}`, `mcp = "prompt"` (a `GET` route returning
  `lesto::mcp::Prompt`) as a prompt whose arguments are the path and query parameters; both take
  `name = ".."` too. It takes effect once the app calls `App::mcp` ([chapter 16](16-mcp.md)).

## Differences to keep in mind

- **Types are checked at compile time.** A `State<Db>` without `with_state(Db)`, a body without
  `Validate`, a handler with the wrong signature: all of it fails at compile time, not on the
  first request.
- **The body goes last** among the handler's arguments (an axum constraint).
- **Every validated field needs a garde attribute**, even just `skip`, or `allow_unvalidated` on
  the struct. Pydantic validates everything by default; garde is explicit.
- **Dependencies are not cached per request** as in FastAPI: if two extractors derive the same
  thing, it is computed twice. If that is expensive, compute it once in a middleware and pass the
  result with `Extension<T>`.
- **Credential verification is yours**, except for OpenID Connect JWTs: security extractors
  extract and document, `Jwt<C>` (feature `oidc`, chapter 9) also verifies.

//! The code shown in `docs/tutorial`, compiled so it stays aligned with the API.
//! Run with `LESTO_PORT=8000 cargo run -p tutorial`.

mod auth;
mod errors;
mod oidc;
mod state;
mod users;

use lesto::axum::extract::{FromRequestParts, Request};
use lesto::axum::middleware::{self, Next};
use lesto::axum::response::{Html, Response};
use lesto::http::request::Parts;
use lesto::prelude::*;
use lesto::{BearerAuth, OperationBuilder, OperationInput, Rejection};

use auth::{AdminKey, CurrentUser, OAuth2, Session, User};
use state::AppState;

// ---- Chapter 1 ------------------------------------------------------------------------------

/// Greets the world.
#[lesto::get("/")]
async fn root() -> &'static str {
    "Hello, lesto!"
}

// ---- Chapter 2 ------------------------------------------------------------------------------

/// Returns the received id.
#[lesto::get("/items/{item_id}")]
async fn read_item(Path(item_id): Path<u64>) -> String {
    format!("item {item_id}")
}

#[lesto::model]
struct UserItemPath {
    /// Id of the owning user.
    user_id: u64,
    /// Id of the item.
    item_id: u64,
}

#[lesto::get("/users/{user_id}/items/{item_id}")]
async fn read_user_item(Path(p): Path<UserItemPath>) -> String {
    format!("user {}, item {}", p.user_id, p.item_id)
}

#[lesto::get("/pairs/{a}/{b}")]
async fn read_pair(Path((a, b)): Path<(u64, u64)>) -> String {
    format!("{a}/{b}")
}

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

// ---- Chapter 3 ------------------------------------------------------------------------------

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
#[lesto::get("/list")]
async fn list_items(Query(q): Query<Pagination>) -> String {
    format!("skip={} limit={}", q.skip, q.limit)
}

#[lesto::model]
#[garde(allow_unvalidated)]
struct Search {
    q: Option<String>,
    #[serde(default)]
    exact: bool,
}

#[lesto::get("/search")]
async fn search(Query(s): Query<Search>) -> String {
    format!("{:?} exact={}", s.q, s.exact)
}

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

// ---- Chapter 4 ------------------------------------------------------------------------------

#[lesto::model]
struct Item {
    /// Item name.
    #[garde(length(min = 1, max = 100))]
    name: String,
    #[garde(skip)]
    #[allow(dead_code)]
    description: Option<String>,
    /// Price in euros.
    #[garde(range(min = 0.0))]
    price: f64,
    #[garde(skip)]
    tax: Option<f64>,
}

#[lesto::model]
#[serde(rename_all = "camelCase")]
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

#[lesto::put("/items/{item_id}")]
async fn update_item(
    Path(item_id): Path<u64>,
    Query(q): Query<Pagination>,
    Json(item): Json<Item>,
) -> String {
    format!(
        "update {item_id} ({} chars, skip={})",
        item.name.len(),
        q.skip
    )
}

// ---- Chapter 5 ------------------------------------------------------------------------------

#[lesto::model]
struct Address {
    #[garde(length(min = 1))]
    street: String,
    #[garde(length(min = 2, max = 2), ascii)]
    country: String,
}

#[lesto::model]
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

fn no_spaces(value: &str, _ctx: &()) -> lesto::garde::Result {
    if value.contains(' ') {
        return Err(lesto::garde::Error::new("must not contain spaces"));
    }
    Ok(())
}

#[lesto::post("/signup", status = 201)]
async fn signup(Json(body): Json<SignUp>) -> String {
    format!("welcome {}", body.username)
}

// ---- Chapter 6 ------------------------------------------------------------------------------

#[lesto::get("/health", public)]
async fn health() -> &'static str {
    "ok"
}

#[lesto::get("/page")]
async fn page() -> Html<String> {
    Html("<h1>hi</h1>".to_string())
}

#[lesto::delete("/items/{item_id}", status = 204)]
async fn delete_item(Path(_item_id): Path<u64>) {}

#[lesto::model]
struct Job {
    #[garde(length(min = 1))]
    task: String,
}

/// Queue a job: the status is in the type.
#[lesto::post("/jobs")]
async fn enqueue(Json(job): Json<Job>) -> Accepted<Json<u64>> {
    Accepted(Json(job.task.len() as u64))
}

#[lesto::delete("/jobs/{id}")]
async fn cancel_job(Path(_id): Path<u64>) -> NoContent {
    NoContent
}

// ---- Chapter 6: one model, several views -------------------------------------------------------

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
    let mut note = Note {
        id: 1,
        author: String::new(),
        text: String::new(),
    };
    note.apply_create(body);
    Json(note)
}

/// Change the text of a note.
#[lesto::patch("/notes/{id}")]
async fn update_note(Path(id): Path<u64>, Json(body): Json<NoteUpdate>) -> Json<Note> {
    let mut note = Note {
        id,
        author: "ada".into(),
        text: "old".into(),
    };
    note.apply_update(body);
    Json(note)
}

// ---- Chapter 7 ------------------------------------------------------------------------------

#[lesto::post("/purchases", responses(402))]
async fn purchase() -> Result<(), HttpError> {
    Err(errors::out_of_credit(30, 50))
}

// ---- Chapter 8 ------------------------------------------------------------------------------

/// Increment and return the counter.
#[lesto::post("/hits")]
async fn hit(State(state): State<AppState>) -> String {
    let mut n = state.counter.lock().unwrap();
    *n += 1;
    n.to_string()
}

/// Profile of the authenticated user.
#[lesto::get("/me")]
async fn me(CurrentUser(user): CurrentUser) -> Json<User> {
    Json(user)
}

pub struct Page {
    pub offset: usize,
    pub limit: usize,
}

impl<S: Send + Sync> FromRequestParts<S> for Page {
    type Rejection = Rejection;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Rejection> {
        let Query(p) = Query::<Pagination>::from_request_parts(parts, state).await?;
        Ok(Page {
            offset: p.skip,
            limit: p.limit,
        })
    }
}

impl OperationInput for Page {
    fn describe(builder: &mut OperationBuilder<'_>) {
        Query::<Pagination>::describe(builder);
    }
}

#[lesto::get("/paged")]
async fn paged(page: Page) -> String {
    format!("{}..{}", page.offset, page.offset + page.limit)
}

// ---- Chapter 9 ------------------------------------------------------------------------------

/// Who is calling?
#[lesto::get("/whoami")]
async fn whoami(auth: Bearer) -> String {
    format!("token: {}", auth.token())
}

/// Delete everything. Requires the admin key.
#[lesto::delete("/everything", status = 204, responses(403))]
async fn nuke(key: ApiKey<AdminKey>) -> Result<(), HttpError> {
    if key.key() != std::env::var("ADMIN_KEY").unwrap_or_default() {
        return Err(HttpError::forbidden("invalid admin key"));
    }
    Ok(())
}

#[lesto::get("/session")]
async fn session(key: ApiKey<Session>) -> String {
    key.into_key()
}

#[lesto::get("/admin")]
async fn admin(creds: Basic) -> Result<String, HttpError> {
    if creds.username != "admin" || creds.password != "s3cret" {
        return Err(HttpError::unauthorized("bad credentials"));
    }
    Ok("welcome".into())
}

#[lesto::get("/oauth-items")]
async fn oauth_items(auth: Bearer<OAuth2>) -> Json<Vec<String>> {
    Json(vec![auth.into_token()])
}

#[lesto::get("/marker")]
async fn marker(_: Security<BearerAuth>) -> &'static str {
    "ok"
}

#[lesto::get("/reports", security(bearerAuth))]
async fn reports() -> &'static str {
    "ok"
}

#[lesto::get("/scoped", security(oauth2 = ["items:write"], "adminKey"))]
async fn scoped() -> &'static str {
    "ok"
}

// ---- Chapter 11 ----------------------------------------------------------------------------

async fn add_request_id(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert("x-request-id", "fixed-id".parse().unwrap());
    res
}

async fn require_json(req: Request, next: Next) -> Result<Response, HttpError> {
    if req.method() == lesto::http::Method::POST
        && !req
            .headers()
            .get("content-type")
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"))
    {
        return Err(HttpError::new(415, "JSON only"));
    }
    Ok(next.run(req).await)
}

async fn legacy_handler() -> &'static str {
    "legacy"
}

/// Chapter 11: lesto's own layers on a router lesto did not build.
pub fn legacy_router() -> lesto::axum::Router {
    use lesto::layers::{CatchPanicLayer, ProblemLayer, RequestSpanLayer};

    lesto::axum::Router::new()
        .route("/legacy", lesto::axum::routing::get(legacy_handler))
        .layer(CatchPanicLayer)
        .layer(ProblemLayer)
        .layer(RequestSpanLayer::new())
}

// ---- Chapter 10: composition -----------------------------------------------------------------

pub fn build_app(state: AppState) -> App<()> {
    let users = App::<AppState>::new().routes(users::routes());

    App::new()
        .title("Tutorial API")
        .version("1.0.0")
        .tag("users", "User management")
        .security_scheme("bearerAuth", SecurityScheme::bearer_jwt())
        .routes(routes![
            root,
            read_item,
            read_user_item,
            read_pair,
            get_model,
            list_items,
            search,
            list_tags,
            create_item,
            update_item,
            signup,
            health,
            page,
            delete_item,
            enqueue,
            cancel_job,
            purchase,
            create_note,
            update_note,
            hit,
            me,
            paged,
            whoami,
            nuke,
            session,
            admin,
            oauth_items,
            marker,
            reports,
            scoped
        ])
        .nest("/users", users)
        .map_router(|router| router.route("/legacy", lesto::axum::routing::get(legacy_handler)))
        // Layers apply to the routes registered before them: they go last.
        .layer(middleware::from_fn(add_request_id))
        .layer(middleware::from_fn(require_json))
        .with_state(state)
}

// ---- Chapter 15 ----------------------------------------------------------------------------

/// The request span, configured: the query string is recorded and the proxy is trusted.
pub fn traced_app(state: AppState) -> App<()> {
    build_app(state).trace(Trace::new().query(true).forwarded(true))
}

// ---- Appendix D: leaving lesto ---------------------------------------------------------------

/// A plain axum handler: no route attribute, still lesto's validating `Json` and `HttpError`.
async fn plain_create_item(Json(item): Json<Item>) -> Result<Json<ItemOut>, HttpError> {
    if item.name == "forbidden" {
        return Err(HttpError::forbidden("that name is taken"));
    }
    Ok(Json(ItemOut {
        name: item.name,
        price_with_tax: item.price + item.tax.unwrap_or(0.0),
    }))
}

/// A router with no `App` at all, keeping lesto's behavior through its public layers.
pub fn plain_router() -> lesto::axum::Router {
    use lesto::layers::{CatchPanicLayer, ProblemLayer, RequestSpanLayer, TimeoutLayer};
    use std::time::Duration;

    lesto::axum::Router::new()
        .route("/items", lesto::axum::routing::post(plain_create_item))
        .layer(TimeoutLayer::new(Duration::from_secs(10)))
        .layer(CatchPanicLayer)
        .layer(ProblemLayer)
        .layer(RequestSpanLayer::new())
}

#[lesto::main]
async fn main() -> std::io::Result<()> {
    // No telemetry code: with the `otel` feature, `serve` reads the `OTEL_*` variables and
    // exports the spans itself (chapter 15).
    build_app(AppState::default()).serve().await
}

// ---- Chapter 12 ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use lesto::axum::body::Body;
    use lesto::http::{Request, StatusCode, header};
    use lesto::serde_json::{self, Value, json};
    use tower::ServiceExt;

    use super::*;

    async fn call(app: App<()>, req: Request<Body>) -> (StatusCode, Value) {
        let response = app.into_router().oneshot(req).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, body)
    }

    fn post_json(uri: &str, body: Value) -> Request<Body> {
        Request::post(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Chapter 15: the configured span does not change what the application answers.
    #[lesto::test]
    async fn a_traced_app_still_serves() {
        let app = traced_app(AppState::for_tests());
        let req = Request::get("/").body(Body::empty()).unwrap();
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "Hello, lesto!");
    }

    #[lesto::test]
    async fn creates_a_user() {
        let app = build_app(AppState::for_tests());
        let req = post_json(
            "/users",
            json!({"name": "Ada", "password": "correct horse"}),
        );
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["name"], "Ada");
        assert!(body.get("password").is_none());
    }

    #[lesto::test]
    async fn repeated_query_parameters_collect_into_a_vec() {
        let app = build_app(AppState::for_tests());
        let req = Request::get("/tags?tag=rust&tag=web")
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "rust,web");
        let (status, body) = call(
            build_app(AppState::for_tests()),
            Request::get("/tags").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, "");
    }

    #[lesto::test]
    async fn rejects_short_passwords() {
        let app = build_app(AppState::for_tests());
        let req = post_json("/users", json!({"name": "Ada", "password": "x"}));
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["errors"][0]["pointer"], "/password");
    }

    #[lesto::test]
    async fn garde_messages_match_the_tutorial() {
        let app = build_app(AppState::for_tests());
        let (_, body) = call(
            app,
            Request::get("/list?limit=500").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(body["errors"][0]["detail"], "greater than 100");

        let app = build_app(AppState::for_tests());
        let (_, body) = call(app, post_json("/items", json!({"name": "", "price": -1}))).await;
        assert_eq!(body["detail"], "2 validation errors");
        assert_eq!(body["errors"][0]["detail"], "length is lower than 1");
        assert_eq!(body["errors"][1]["detail"], "lower than 0");

        let app = build_app(AppState::for_tests());
        let (_, body) = call(app, post_json("/items", json!({"name": "Tastiera"}))).await;
        assert_eq!(body["errors"][0]["code"], "missing");
        assert_eq!(body["errors"][0]["pointer"], "/price");

        let app = build_app(AppState::for_tests());
        let (status, body) = call(
            app,
            post_json(
                "/signup",
                json!({
                    "username": "ada", "email": "ada@example.com",
                    "password": "longenough", "password_confirm": "longenough",
                    "addresses": [{"street": "Via Roma", "country": "IT"}, {"street": "", "country": "IT"}],
                    "display_name": "Ada"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["errors"][0]["pointer"], "/addresses/1/street");
    }

    #[lesto::test]
    async fn path_examples() {
        let app = build_app(AppState::for_tests());
        let (status, body) =
            call(app, Request::get("/items/foo").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["errors"][0]["pointer"], "/item_id");
        assert_eq!(body["errors"][0]["detail"], "Cannot parse `foo` to a `u64`");

        let app = build_app(AppState::for_tests());
        let (_, body) = call(
            app,
            Request::get("/models/alexnet").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(body, "Deep Learning FTW!");
    }

    #[lesto::test]
    async fn current_user_and_security() {
        let app = build_app(AppState::for_tests());
        let req = Request::get("/me")
            .header("authorization", "Bearer secret")
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"id": 1, "name": "Ada"}));

        let app = build_app(AppState::for_tests());
        let req = Request::get("/me")
            .header("authorization", "Bearer wrong")
            .body(Body::empty())
            .unwrap();
        let (status, _) = call(app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let spec = serde_json::to_value(build_app(AppState::for_tests()).openapi()).unwrap();
        assert_eq!(
            spec["paths"]["/me"]["get"]["security"],
            json!([{"bearerAuth": []}])
        );
        assert_eq!(
            spec["paths"]["/oauth-items"]["get"]["security"],
            json!([{"oauth2": ["items:read"]}])
        );
        assert_eq!(
            spec["paths"]["/scoped"]["get"]["security"],
            json!([{"oauth2": ["items:write"]}, {"adminKey": []}])
        );
        assert_eq!(spec["paths"]["/health"]["get"]["security"], json!([]));
        assert_eq!(
            spec["components"]["securitySchemes"]["bearerAuth"]["bearerFormat"],
            "JWT"
        );
        assert_eq!(
            spec["components"]["securitySchemes"]["session"]["in"],
            "cookie"
        );
    }

    #[lesto::test]
    async fn middleware_and_legacy_route() {
        let app = build_app(AppState::for_tests());
        let response = app
            .into_router()
            .oneshot(Request::get("/legacy").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-request-id"], "fixed-id");

        let app = build_app(AppState::for_tests());
        let (status, body) = call(app, Request::post("/hits").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(body["detail"], "JSON only");
    }

    #[lesto::test]
    async fn openapi_contract() {
        let spec = serde_json::to_value(build_app(AppState::for_tests()).openapi()).unwrap();
        assert!(spec["paths"]["/users"]["post"].is_object());
        assert_eq!(
            spec["paths"]["/users"]["post"]["responses"]["201"]["content"]["application/json"]["schema"]
                ["$ref"],
            "#/components/schemas/UserOut"
        );
        let limit = &spec["paths"]["/list"]["get"]["parameters"][1];
        assert_eq!(limit["name"], "limit");
        assert_eq!(limit["schema"]["minimum"], 1);
        assert_eq!(limit["schema"]["maximum"], 100);
        assert_eq!(limit["schema"]["default"], 10);
        assert_eq!(
            spec["components"]["schemas"]["ItemOut"]["properties"]["priceWithTax"]["type"],
            "number"
        );
        assert_eq!(
            spec["paths"]["/purchases"]["post"]["responses"]["402"]["description"],
            "Payment Required"
        );
    }

    #[lesto::test]
    async fn views_create_and_patch() {
        let app = build_app(AppState::for_tests());
        let (status, body) = call(
            app,
            post_json("/notes", json!({"author": "Ada", "text": "hi"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body, json!({"id": 1, "author": "Ada", "text": "hi"}));

        let app = build_app(AppState::for_tests());
        let req = Request::patch("/notes/9")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({}).to_string()))
            .unwrap();
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["text"], "old");

        let spec = serde_json::to_value(build_app(AppState::for_tests()).openapi()).unwrap();
        // Optional view fields are nullable in the schema; garde `inner(..)` rules are enforced
        // at runtime but schemars does not mirror them into the schema.
        assert_eq!(
            spec["components"]["schemas"]["NoteUpdate"]["properties"]["text"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(
            spec["components"]["schemas"]["NoteCreate"]["properties"]["text"]["minLength"],
            1
        );
    }

    /// Appendix D: a plain axum router keeps validation and problems.
    #[lesto::test]
    async fn a_plain_router_keeps_validation_and_problems() {
        let send = |body: Value| {
            plain_router().oneshot(
                Request::post("/items")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
        };
        let response = send(json!({"name": "Keyboard", "price": 45.0}))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = send(json!({"name": "", "price": 45.0})).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["instance"], "/items");

        let response = send(json!({"name": "forbidden", "price": 1.0}))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Chapter 6: the status comes from the type, at runtime and in the document.
    #[lesto::test]
    async fn status_types() {
        let (status, body) = call(
            build_app(AppState::for_tests()),
            post_json("/jobs", json!({"task": "mail"})),
        )
        .await;
        assert_eq!((status, body), (StatusCode::ACCEPTED, json!(4)));
        let spec = serde_json::to_value(build_app(AppState::for_tests()).openapi()).unwrap();
        assert!(spec["paths"]["/jobs"]["post"]["responses"]["202"].is_object());
        assert!(spec["paths"]["/jobs/{id}"]["delete"]["responses"]["204"].is_object());
    }

    #[lesto::test]
    async fn health_says_ok() {
        assert_eq!(health().await, "ok");
    }
}

# C. Why axum and not actix-web

lesto is built on [axum](https://github.com/tokio-rs/axum). The other natural candidate was
[actix-web](https://actix.rs), the longest-lived Rust framework and often at the top of the
benchmarks. Both are mature, fast and in production at many companies: the choice is not "one is
better" but "one fits what lesto wants to be". Here are the reasons, heaviest first.

## 1. State is checked by the compiler

In axum, shared state is a type parameter of the router: `Router<S>` and `State<S>` must match,
otherwise the program **does not compile**. In actix-web, state is registered with
`app_data(web::Data::new(...))` and extracted with `web::Data<T>`: forget the registration, or
register a different type, and you find out **at runtime** with a `500` on the first request.

lesto promises that configuration mistakes surface at compile time, like body and response
types do. axum's typed state makes that promise complete; with actix-web it would have been an
exception to document.

## 2. tower: middleware shared with the whole ecosystem

axum has no middleware system of its own: it uses [tower](https://github.com/tower-rs/tower). A
layer written for tower works with axum, with hyper, with tonic (gRPC), with any service.
[tower-http](https://docs.rs/tower-http) already provides CORS, compression, timeouts, tracing,
authentication, size limits, static files.

actix-web has its own middleware system, well made but closed: middleware is written for
actix-web and stays there. lesto wants to be a thin layer that does not isolate you from the
ecosystem, and tower is that ecosystem.

## 3. A router is a `Service`

`App::into_router()` returns an `axum::Router`, which is a `tower::Service`. You can serve it with
`axum::serve`, with hyper directly, with `axum-server` for TLS, inside a Lambda with
`lambda_http`, merged with a gRPC router. lesto does not have to care how the service is run: it
leaves that to axum, which leaves it to tower. In actix-web the server, the runtime and the router
are one piece.

## 4. The runtime is tokio, full stop

axum runs on tokio, the runtime used by `sqlx`, `reqwest`, `tonic`, `redis`, almost everything. A
handler is an ordinary `Send` future and combines with any async library without a second thought.

actix-web uses `actix-rt`, built on tokio but with a **single-threaded worker** model: every
worker has its own runtime and handlers may be `!Send`. It is a choice with real advantages, for
instance using `Rc` and `RefCell` inside a handler, but it introduces rules that beginners meet as
surprises: `tokio::spawn` inside a handler needs care, some libraries must be initialized per
worker, tests must use `#[actix_web::test]` instead of `#[lesto::test]`. For a framework aimed at
beginners, the road without exceptions matters more than the performance margin.

## 5. The extractor model is FastAPI's

Both frameworks extract handler arguments from types (`FromRequest` in actix-web,
`FromRequestParts` / `FromRequest` in axum). In axum, however, the `Handler<T, S>` trait exposes
the tuple `T` of argument types as a generic parameter: lesto intercepts it and asks each type to
describe itself in OpenAPI (`OperationInput`), without a macro having to read the function
signature. That mechanism is what makes `routes![handler]` enough to document everything.

The same can be achieved in actix-web, but with more code in the macro and less in the type
system.

## 6. The things that did *not* weigh

- **Performance.** actix-web is almost always ahead in synthetic benchmarks; axum is close. In a
  real API the time goes to the database and the network: the difference between the two
  frameworks is in the order of microseconds per request and disappears into the noise.
- **Path syntax.** Both use `{name}`, which matches OpenAPI.
- **Maturity and community.** Both are active and stable. axum lives in the tokio-rs
  organization; actix-web has an independent, established team.
- **Existing OpenAPI support.** `utoipa` supports both; `aide` only axum. lesto generates its own
  document and depends on neither.

## 7. The costs of the choice

- Handlers must be `Send`: no `Rc`/`RefCell` across an `.await`. In practice you use `Arc` and
  `Mutex`/`RwLock`, or better, types that are already thread-safe such as connection pools.
- Middleware written for actix-web cannot be reused.
- In pure benchmarks a few percentage points are left on the table.

## Could lesto support actix-web too?

lesto's core is framework-independent: the OpenAPI model, the `OperationInput` /
`OperationOutput` traits, `HttpError` and the RFC 9457 format, the conversion of garde reports. The
layer binding them to axum, that is the extractors, `RouteSet` and `App`, is about a third of the
code. An actix-web backend would therefore be possible, with its own extractors and its own `App`.
It is not planned: lesto's value lies in being one simple way of building APIs in Rust, and two
backends would double the surface to learn and to maintain.

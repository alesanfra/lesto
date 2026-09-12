//! The smallest lesto application: one route, documentation at `/docs`.
//!
//! ```sh
//! LESTO_PORT=8765 cargo run -p hello      # or: lesto dev -p hello --port 8765
//! curl http://127.0.0.1:8765/
//! ```

use lesto::prelude::*;

/// Say hello.
#[lesto::get("/hello")]
async fn hello() -> &'static str {
    "Hello, lesto!"
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Binds LESTO_HOST:LESTO_PORT (default 127.0.0.1:8000), or the socket `lesto dev` hands over.
    App::new()
        .title("Hello")
        .routes(routes![hello])
        .serve()
        .await
}

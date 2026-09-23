//! Graceful shutdown: in-flight requests finish, then `serve` returns.

use std::time::Duration;

use lesto::prelude::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[lesto::get("/slow")]
async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(300)).await;
    "done"
}

#[tokio::test]
async fn in_flight_requests_finish_before_serve_returns() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = App::<()>::new().routes(routes![slow]);
    let server = tokio::spawn(app.serve_until(listener, async {
        rx.await.ok();
    }));

    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    // The request is being handled: ask the server to stop now.
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(()).unwrap();

    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("done"), "{response}");

    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve returns once the in-flight request is done")
        .unwrap();
    assert!(result.is_ok());
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "the listener is closed after shutdown"
    );
}

#[lesto::get("/hang")]
async fn hang() -> &'static str {
    std::future::pending::<()>().await;
    "never"
}

#[tokio::test]
async fn a_hung_handler_does_not_outlive_the_shutdown_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = App::<()>::new()
        .routes(routes![hang])
        .shutdown_timeout(Duration::from_millis(200));
    let server = tokio::spawn(app.serve_until(listener, async {
        rx.await.ok();
    }));

    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /hang HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(()).unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve returns at the deadline even though the handler never does")
        .unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn without_a_deadline_shutdown_waits_for_the_handler() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app = App::<()>::new()
        .routes(routes![hang])
        .shutdown_timeout(None);
    let server = tokio::spawn(app.serve_until(listener, async {
        rx.await.ok();
    }));

    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /hang HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(()).unwrap();

    assert!(
        tokio::time::timeout(Duration::from_millis(500), server)
            .await
            .is_err(),
        "with no deadline the server keeps waiting for the hung request"
    );
}

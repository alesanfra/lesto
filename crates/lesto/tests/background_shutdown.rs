//! `serve_until` and background tasks. Its own binary, and the tests take a lock: the tracker
//! they wait on is the process's, so one test's task must not be another's.

use std::time::Duration;

use lesto::prelude::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, oneshot};

static SERIAL: Mutex<()> = Mutex::const_new(());

/// `serve_until` prints on stdout when no subscriber is installed; keep `cargo test` readable.
fn quiet() {
    let _ = tracing::subscriber::set_global_default(tracing::subscriber::NoSubscriber::default());
}

#[derive(Clone)]
struct Gate(std::sync::Arc<Mutex<Option<oneshot::Receiver<()>>>>);

#[lesto::get("/start")]
async fn start(tasks: BackgroundTasks, State(gate): State<Gate>) -> &'static str {
    let release = gate.0.lock().await.take().expect("one task per test");
    tasks.spawn(async move {
        release.await.ok();
    });
    "started"
}

struct Running {
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    stop: oneshot::Sender<()>,
    release: oneshot::Sender<()>,
}

/// Serves the app, requests `/start` (which spawns a task waiting for `release`), and asks the
/// server to stop.
async fn serve(timeout: impl Into<Option<Duration>>) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let (release, released) = oneshot::channel::<()>();
    let gate = Gate(std::sync::Arc::new(Mutex::new(Some(released))));
    let app = App::<Gate>::new()
        .routes(routes![start])
        .with_state::<()>(gate)
        .shutdown_timeout(timeout.into());
    quiet();
    let server = tokio::spawn(app.serve_until(listener, async {
        stopped.await.ok();
    }));
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /start HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    Running {
        server,
        stop,
        release,
    }
}

#[tokio::test]
async fn shutdown_waits_for_a_running_task() {
    let _serial = SERIAL.lock().await;
    let Running {
        mut server,
        stop,
        release,
    } = serve(Duration::from_secs(10)).await;
    stop.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut server)
            .await
            .is_err(),
        "serve is waiting for the task"
    );
    release.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve returns once the task is done")
        .unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn shutdown_gives_up_on_a_task_at_the_deadline() {
    let _serial = SERIAL.lock().await;
    let Running {
        server,
        stop,
        release,
    } = serve(Duration::from_millis(300)).await;
    stop.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve returns at the deadline though the task never finishes")
        .unwrap();
    assert!(result.is_ok());
    // Let the abandoned task end so the next test starts clean.
    release.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn without_a_deadline_shutdown_waits_for_the_task_unbounded() {
    let _serial = SERIAL.lock().await;
    let Running {
        mut server,
        stop,
        release,
    } = serve(None).await;
    stop.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), &mut server)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve returns once the task is done")
        .unwrap()
        .unwrap();
}

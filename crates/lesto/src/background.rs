//! Work that runs after the response: [`BackgroundTasks`].
//!
//! **Tasks are best effort, not durable.** A task lives in the memory of one process: a crash,
//! an out-of-memory kill, a deploy that outlasts the shutdown deadline or a Lambda timeout
//! loses it, and nothing retries it. Work that must happen belongs in a queue or a database
//! outbox. Use tasks for what is nice to have done soon: a confirmation email, a cache warm-up,
//! an audit line.
//!
//! ```
//! use lesto::prelude::*;
//!
//! #[lesto::post("/orders", status = 201)]
//! async fn create(tasks: BackgroundTasks) -> &'static str {
//!     tasks.spawn(async move {
//!         // runs after the response, concurrently with other tasks
//!     });
//!     "created"
//! }
//! ```
//!
//! # Semantics
//!
//! - [`BackgroundTasks::spawn`] starts the task at once on the runtime and returns; the task
//!   outlives the request. Many tasks run concurrently, in no particular order.
//! - A task returns `()` or a `Result<(), E>` with `E: Display`; an `Err` is logged at `ERROR`
//!   (target `lesto::background`). A panic is caught, logged and never reaches the request.
//! - Each task runs in a span `background {name}` (`otel.name`), a child of the span current
//!   when it was spawned (the request span, in a handler). `{name}` is the function the task
//!   was written in.
//! - [`App::serve`](crate::App::serve) waits for running tasks after the server stopped, inside
//!   the same [`shutdown_timeout`](crate::App::shutdown_timeout) as the requests; tasks still
//!   running at the deadline are abandoned with a warning.
//!
//! Not available on `wasm32`.

use std::any::Any;
use std::convert::Infallible;
use std::fmt::Display;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use axum::extract::FromRequestParts;
use http::request::Parts;
use pin_project_lite::pin_project;
use tokio::sync::Notify;
use tracing::{Instrument, Span};

use crate::layers::panic_message;
use crate::operation::OperationInput;

/// What a background task may return: `()`, or a `Result<(), E>` whose error is logged.
///
/// Implemented by lesto for those two types; there is nothing to implement.
pub trait TaskOutput: Send + 'static {
    /// Reports the output: nothing for `()`, an `ERROR` event for an `Err`.
    #[doc(hidden)]
    fn finish(self);
}

impl TaskOutput for () {
    fn finish(self) {}
}

impl<E: Display + Send + 'static> TaskOutput for Result<(), E> {
    fn finish(self) {
        if let Err(error) = self {
            tracing::error!(target: "lesto::background", error = %error, "background task failed");
        }
    }
}

// ---- the tracker ----------------------------------------------------------------------------

/// Counts the tasks that have not finished and lets a caller wait for zero.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tracker(Arc<TrackerInner>);

#[derive(Debug, Default)]
struct TrackerInner {
    live: AtomicUsize,
    idle: Notify,
}

impl Tracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Registers a task; it counts as running until the guard is dropped (finished, panicked or
    /// dropped with the runtime).
    pub(crate) fn start(&self) -> Guard {
        self.0.live.fetch_add(1, Ordering::AcqRel);
        Guard(self.0.clone())
    }

    /// Tasks not finished yet.
    pub(crate) fn live(&self) -> usize {
        self.0.live.load(Ordering::Acquire)
    }

    /// Returns once no task is running (at once when none is).
    pub(crate) async fn wait(&self) {
        loop {
            // Registered before the count is read, so a guard dropping in between still wakes
            // this waiter.
            let notified = self.0.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.live() == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// Marks one task as running.
pub(crate) struct Guard(Arc<TrackerInner>);

impl Drop for Guard {
    fn drop(&mut self) {
        if self.0.live.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

/// The tracker of the process: where tasks go unless the request carries a
/// [`BackgroundScope`].
pub(crate) fn global() -> &'static Tracker {
    static GLOBAL: OnceLock<Tracker> = OnceLock::new();
    GLOBAL.get_or_init(Tracker::new)
}

/// Request extension naming the tracker the request's [`BackgroundTasks`] use, instead of the
/// process-wide one (one per Lambda invocation).
#[derive(Clone, Debug)]
pub(crate) struct BackgroundScope(pub(crate) Tracker);

// ---- the extractor --------------------------------------------------------------------------

/// Starts work that runs after the response has been sent.
///
/// **Tasks are best effort, not durable**: they live in the memory of one process, and a crash,
/// a kill or a timeout loses them without a retry. Anything that must happen belongs in a queue
/// or an outbox. See the [module](self) for the semantics.
///
/// A handler argument that adds nothing to the OpenAPI document, valid in any position.
#[derive(Clone, Debug)]
pub struct BackgroundTasks {
    tracker: Tracker,
}

impl BackgroundTasks {
    /// Runs `task` after the response: starts it at once and returns, the task outlives the
    /// request.
    ///
    /// The task runs in a span `background {name}`, a child of the current one. If it returns
    /// an `Err` or panics, that is logged at `ERROR` and nothing else happens: the response
    /// has been sent already.
    ///
    /// # Panics
    ///
    /// Outside a Tokio runtime, like [`tokio::spawn`].
    pub fn spawn<F>(&self, task: F)
    where
        F: Future + Send + 'static,
        F::Output: TaskOutput,
    {
        let span = task_span::<F>();
        let guard = self.tracker.start();
        tokio::spawn(
            async move {
                let _guard = guard;
                match (CatchUnwind { future: task }).await {
                    Ok(output) => output.finish(),
                    Err(payload) => tracing::error!(
                        target: "lesto::background",
                        panic = %panic_message(payload.as_ref()),
                        "background task panicked"
                    ),
                }
            }
            .instrument(span),
        );
    }
}

fn task_span<F>() -> Span {
    let name = crate::trace::operation_of::<F>("");
    let span = tracing::info_span!(
        "background",
        otel.name = tracing::field::Empty,
        otel.kind = "internal",
    );
    if !span.is_disabled() {
        let otel_name = if name.is_empty() {
            "background".to_string()
        } else {
            format!("background {name}")
        };
        span.record("otel.name", otel_name.as_str());
    }
    span
}

impl<S: Send + Sync> FromRequestParts<S> for BackgroundTasks {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Infallible> {
        let tracker = match parts.extensions.get::<BackgroundScope>() {
            Some(scope) => scope.0.clone(),
            None => global().clone(),
        };
        Ok(Self { tracker })
    }
}

impl OperationInput for BackgroundTasks {}

pin_project! {
    /// `catch_unwind` around every poll, with the panic as the output.
    struct CatchUnwind<F> {
        #[pin]
        future: F,
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn Any + Send>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let future = self.project().future;
        match catch_unwind(AssertUnwindSafe(|| future.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn name_of<F: Future>(_task: &F) -> String {
        crate::trace::operation_of::<F>("").into_owned()
    }

    fn explode() {
        panic!("boom");
    }

    async fn handler() -> String {
        let task = async move { 1 };
        name_of(&task)
    }

    #[tokio::test]
    async fn an_async_block_is_named_after_the_function_it_was_written_in() {
        assert_eq!(handler().await, "tests::handler");
    }

    #[tokio::test]
    async fn wait_returns_when_the_last_guard_drops_even_after_a_panic() {
        let tracker = Tracker::new();
        tracker.wait().await;
        let tasks = BackgroundTasks {
            tracker: tracker.clone(),
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tasks.spawn(async move {
            rx.await.ok();
            explode();
        });
        assert_eq!(tracker.live(), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), tracker.wait())
                .await
                .is_err()
        );
        tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), tracker.wait())
            .await
            .unwrap();
        assert_eq!(tracker.live(), 0);
    }
}

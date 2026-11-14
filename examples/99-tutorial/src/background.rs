// Chapter 17: background tasks, work that runs after the response
use std::fmt;
use std::time::Duration;

use lesto::prelude::*;
use lesto::tokio::sync::mpsc;

#[lesto::model]
pub struct Order {
    /// What was bought.
    #[garde(length(min = 1))]
    item: String,
}

#[lesto::model]
pub struct Created {
    id: u64,
}

/// A stand-in for an email service: it records what it was asked to send.
#[derive(Clone)]
pub struct Mailer {
    sent: mpsc::UnboundedSender<String>,
    failing: bool,
}

#[derive(Debug)]
pub struct MailError(String);

impl fmt::Display for MailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mail service unavailable: {}", self.0)
    }
}

impl Mailer {
    async fn send(&self, to: &str) -> Result<(), MailError> {
        // A slow call: the customer must not wait for it.
        lesto::tokio::time::sleep(Duration::from_millis(50)).await;
        if self.failing {
            return Err(MailError(to.to_owned()));
        }
        self.sent.send(to.to_owned()).ok();
        Ok(())
    }
}

/// Create an order; the confirmation is sent after the response.
#[lesto::post("/orders", status = 201)]
async fn create_order(
    tasks: BackgroundTasks,
    State(mailer): State<Mailer>,
    Json(order): Json<Order>,
) -> Json<Created> {
    let created = Created { id: 1 };
    tasks.spawn(async move {
        // Runs after the response; an Err is logged, the customer never sees it.
        mailer.send(&order.item).await
    });
    Json(created)
}

/// A task that returns `()` and one that panics: neither reaches the caller.
#[lesto::post("/audit", status = 202)]
async fn audit(tasks: BackgroundTasks) -> &'static str {
    tasks.spawn(async move {
        lesto::tracing::info!("audit line written");
    });
    tasks.spawn(async move {
        let _: u32 = "not a number".parse().unwrap(); // a bug: panics
    });
    "accepted"
}

pub fn orders_app(mailer: Mailer) -> App {
    App::<Mailer>::new()
        .routes(routes![create_order, audit])
        .with_state(mailer)
}

#[allow(dead_code)]
pub fn mailer() -> Mailer {
    Mailer {
        sent: mpsc::unbounded_channel().0,
        failing: false,
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use lesto::axum::body::Body;
    use lesto::http::{Request, StatusCode, header};
    use lesto::serde_json::json;
    use tower::ServiceExt;

    use super::*;

    fn post(uri: &str, body: String) -> Request<Body> {
        Request::post(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    // The chapter's test: oneshot, then wait on what the task writes.
    #[lesto::test]
    async fn the_task_runs_after_the_response() {
        let (tx, mut sent) = mpsc::unbounded_channel();
        let mailer = Mailer {
            sent: tx,
            failing: false,
        };
        let router = orders_app(mailer).into_router();

        let body = json!({"item": "book"}).to_string();
        let response = router.oneshot(post("/orders", body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        // The response is out; the mail is not sent yet (the task sleeps first).
        assert!(sent.try_recv().is_err());

        let to = lesto::tokio::time::timeout(Duration::from_secs(5), sent.recv())
            .await
            .expect("the task never ran")
            .unwrap();
        assert_eq!(to, "book");
    }

    #[lesto::test]
    async fn an_error_in_the_task_does_not_change_the_response() {
        let (tx, mut sent) = mpsc::unbounded_channel();
        let mailer = Mailer {
            sent: tx,
            failing: true,
        };
        let router = orders_app(mailer).into_router();

        let body = json!({"item": "book"}).to_string();
        let response = router.oneshot(post("/orders", body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], br#"{"id":1}"#);

        // The task failed: it logged, and sent nothing.
        lesto::tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(sent.try_recv().is_err());
    }

    #[lesto::test]
    async fn a_panic_in_a_task_does_not_reach_the_caller() {
        let router = orders_app(mailer()).into_router();
        let response = router.oneshot(post("/audit", String::new())).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
}

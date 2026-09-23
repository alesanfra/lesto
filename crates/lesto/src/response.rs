//! Response types that carry their status: [`Created`], [`Accepted`], [`NoContent`].
//!
//! The status comes from the type, so a route can answer `201` on one path and `200` on
//! another, and the OpenAPI document shows the status the handler really returns. The
//! `status = N` route option stays as a shortcut for a handler with one success status.

use axum::response::{IntoResponse, Response};
use http::StatusCode;

use crate::{OperationBuilder, OperationOutput};

/// `201 Created` around another response: `Created(Json(note))`.
///
/// Documented as the inner type's response under `201`. The status is applied only to a
/// successful inner response, so a body that fails to serialize still answers `500`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Created<T = ()>(pub T);

/// `202 Accepted` around another response: the work was queued, not done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Accepted<T = ()>(pub T);

/// `204 No Content`, with no body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoContent;

fn with_status(inner: impl IntoResponse, status: StatusCode) -> Response {
    let mut response = inner.into_response();
    if response.status() == StatusCode::OK {
        *response.status_mut() = status;
    }
    response
}

impl<T: IntoResponse> IntoResponse for Created<T> {
    fn into_response(self) -> Response {
        with_status(self.0, StatusCode::CREATED)
    }
}

impl<T: IntoResponse> IntoResponse for Accepted<T> {
    fn into_response(self) -> Response {
        with_status(self.0, StatusCode::ACCEPTED)
    }
}

impl IntoResponse for NoContent {
    fn into_response(self) -> Response {
        StatusCode::NO_CONTENT.into_response()
    }
}

impl<T: OperationOutput> OperationOutput for Created<T> {
    fn describe(builder: &mut OperationBuilder<'_>, _status: u16) {
        T::describe(builder, 201);
    }
}

impl<T: OperationOutput> OperationOutput for Accepted<T> {
    fn describe(builder: &mut OperationBuilder<'_>, _status: u16) {
        T::describe(builder, 202);
    }
}

impl OperationOutput for NoContent {
    fn describe(builder: &mut OperationBuilder<'_>, _status: u16) {
        builder.empty_response(204u16, "No Content");
    }
}

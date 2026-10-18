//! Request validation for hornero, the counterpart of Nest's `ValidationPipe`.
//!
//! Rust already parses requests into types; `Valid<E>` adds the rules serde cannot express:
//!
//! ```ignore
//! #[derive(Deserialize, Validate)]
//! struct CreateTodo {
//!     #[garde(length(min = 1, max = 200))]
//!     title: String,
//! }
//!
//! #[post("/")]
//! async fn create(&self, Valid(Json(input)): Valid<Json<CreateTodo>>) -> ... { }
//! ```
//!
//! Add `garde` (with the `derive` feature) to your own dependencies: its derive macro refers to
//! `::garde` paths.
//!
//! Failures become a 422 `HttpError` listing every field error; malformed input (bad JSON, a
//! missing query parameter) becomes an `HttpError` with the extractor's status. Both render as
//! problem+json and reach exception filters like any other error.

#![warn(missing_docs)]

use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
pub use garde;
use garde::Validate;
use hornero::HttpError;
use serde_json::json;

/// Runs `garde` validation on the value extracted by `E`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Valid<E>(pub E);

/// Extractors whose payload can be validated.
pub trait Validated {
    /// The validated payload type.
    type Target: Validate;

    /// The payload to validate.
    fn target(&self) -> &Self::Target;
}

impl<T: Validate> Validated for Json<T> {
    type Target = T;
    fn target(&self) -> &T {
        &self.0
    }
}

impl<T: Validate> Validated for Query<T> {
    type Target = T;
    fn target(&self) -> &T {
        &self.0
    }
}

impl<T: Validate> Validated for Form<T> {
    type Target = T;
    fn target(&self) -> &T {
        &self.0
    }
}

impl<T: Validate> Validated for Path<T> {
    type Target = T;
    fn target(&self) -> &T {
        &self.0
    }
}

impl<S, E> FromRequest<S> for Valid<E>
where
    S: Send + Sync,
    E: FromRequest<S> + Validated,
    <E::Target as Validate>::Context: Default,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        // Turn the rejection into a `Response` before awaiting: `E::Rejection` need not be `Send`.
        let rejection = match E::from_request(req, state).await {
            Ok(value) => {
                check(value.target()).map_err(IntoResponse::into_response)?;
                return Ok(Valid(value));
            }
            Err(rejection) => rejection.into_response(),
        };
        Err(reject(rejection).await)
    }
}

impl<S, E> FromRequestParts<S> for Valid<E>
where
    S: Send + Sync,
    E: FromRequestParts<S> + Validated,
    <E::Target as Validate>::Context: Default,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let rejection = match E::from_request_parts(parts, state).await {
            Ok(value) => {
                check(value.target()).map_err(IntoResponse::into_response)?;
                return Ok(Valid(value));
            }
            Err(rejection) => rejection.into_response(),
        };
        Err(reject(rejection).await)
    }
}

fn check<T: Validate>(value: &T) -> Result<(), HttpError>
where
    T::Context: Default,
{
    value.validate().map_err(|report| {
        let errors: Vec<_> = report
            .iter()
            .map(|(path, error)| json!({ "path": path.to_string(), "message": error.message() }))
            .collect();
        HttpError::unprocessable("validation failed").with_details(errors)
    })
}

/// Re-renders an axum rejection (plain text) as an `HttpError`, so every client error looks alike.
async fn reject(rejection: Response) -> Response {
    let status = rejection.status();
    let message = match axum::body::to_bytes(rejection.into_body(), 64 * 1024).await {
        Ok(bytes) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).into_owned(),
        _ => status
            .canonical_reason()
            .unwrap_or("invalid request")
            .to_owned(),
    };
    HttpError::new(status, message).into_response()
}

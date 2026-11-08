//! Global interceptor and error format, registered on the `Hornero` builder in `lib.rs`.

use std::time::Instant;

use hornero::axum::extract::Request;
use hornero::axum::http::header::CONTENT_TYPE;
use hornero::prelude::*;
use serde_json::json;

/// Logs every handled route with its handler name, something a plain tower layer cannot know.
#[derive(Injectable)]
pub struct LoggingInterceptor;

impl Interceptor for LoggingInterceptor {
    async fn intercept(
        &self,
        ctx: &RouteContext,
        req: Request,
        next: CallHandler,
    ) -> Result<Response, HttpError> {
        let started = Instant::now();
        let result = next.handle(req).await;
        let status = match &result {
            Ok(response) => response.status(),
            Err(err) => err.status(),
        };
        tracing::info!(
            method = %ctx.method(),
            path = ctx.path(),
            handler = format!("{}::{}", ctx.controller(), ctx.handler()),
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "handled request"
        );
        result
    }
}

/// The application's error format: the default problem document plus the route that failed.
pub fn problem(err: &HttpError, ctx: &RouteContext) -> Response {
    let mut problem = err.to_problem();
    problem["instance"] = json!(ctx.path());
    problem["handler"] = json!(format!("{}::{}", ctx.controller(), ctx.handler()));
    (
        err.status(),
        [(CONTENT_TYPE, "application/problem+json")],
        problem.to_string(),
    )
        .into_response()
}

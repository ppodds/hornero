//! Global enhancers, registered on the `Hornero` builder in `lib.rs`.

use std::time::Instant;

use hornero::axum::extract::Request;
use hornero::axum::http::header::CONTENT_TYPE;
use hornero::axum::middleware::Next;
use hornero::prelude::*;
use serde_json::json;

/// Logs every handled route with its handler name, something a plain tower layer cannot know.
#[derive(Injectable)]
pub struct LoggingInterceptor;

impl Interceptor for LoggingInterceptor {
    async fn intercept(&self, ctx: &RouteContext, req: Request, next: Next) -> Response {
        let started = Instant::now();
        let response = next.run(req).await;
        tracing::info!(
            method = %ctx.method(),
            path = ctx.path(),
            handler = format!("{}::{}", ctx.controller(), ctx.handler()),
            status = response.status().as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "handled request"
        );
        response
    }
}

/// Extends the default problem document with the route that failed.
#[derive(Injectable)]
pub struct ProblemFilter;

impl ExceptionFilter for ProblemFilter {
    async fn catch(&self, err: &HttpError, ctx: &RouteContext) -> Response {
        problem(err, ctx)
    }
}

/// The application's error format. Filters that map domain errors render through it too, since
/// only one filter handles each error.
pub fn problem(err: &HttpError, ctx: &RouteContext) -> Response {
    let mut problem = err.to_problem();
    problem["instance"] = json!(ctx.path());
    problem["handler"] = json!(format!("{}::{}", ctx.controller(), ctx.handler()));
    let mut response = (
        err.status(),
        [(CONTENT_TYPE, "application/problem+json")],
        problem.to_string(),
    )
        .into_response();
    // Rebuilding the response drops what the error carried, `WWW-Authenticate` on a 401.
    response.headers_mut().extend(err.headers().clone());
    response
}

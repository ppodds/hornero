//! The axum layers around routes: 405 and 404 answers, DI middleware and the 5xx log. A route's
//! own guards, interceptors and filters run in [`chain`](super::chain).

use std::sync::Arc;

use axum::Router;
use axum::body::HttpBody;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;

use super::middleware::ErasedMiddleware;
use crate::error::{ErrorExtension, HttpError};

/// Whether `response` is axum's own answer with `status`: no body, no content type and no
/// [`HttpError`] behind it. A handler or fallback that answers with a body or an error keeps it.
fn is_bare(response: &Response, status: StatusCode) -> bool {
    response.status() == status
        && response.body().is_end_stream()
        && !response.headers().contains_key(header::CONTENT_TYPE)
        && response.extensions().get::<ErrorExtension>().is_none()
}

/// Renders axum's bare 405 as problem+json.
///
/// axum adds the `Allow` header itself, after the layers, to whatever the fallback returns, so it
/// survives the replacement. This wraps the whole method router (fallback included), so it is the
/// one place that uses `layer` instead of `route_layer`.
pub(crate) fn with_problem_405(route: MethodRouter) -> MethodRouter {
    route.layer(from_fn(|req: Request, next: Next| async move {
        let response = next.run(req).await;
        if !is_bare(&response, StatusCode::METHOD_NOT_ALLOWED) {
            return response;
        }
        HttpError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed for this route",
        )
        .into_response()
    }))
}

/// The application's fallback, for requests no route matches: the fallback of a router mounted at
/// `/` (an SPA's `index.html`, say), else hornero's problem+json 404.
///
/// It runs outside every module's layers and middleware, which only wrap that module's routes.
/// The mounted routers' routes are already in the application, inside their module's layers; here
/// they answer a bare 404, so a request that a global layer rewrote after routing cannot reach one
/// around those layers.
pub(crate) fn root_fallback(root_mounts: Vec<Router>) -> Router {
    let mut fallback = root_mounts.into_iter().fold(Router::new(), Router::merge);
    // `route_layer` panics on a router without routes, such as a mount that is only a fallback.
    if fallback.has_routes() {
        fallback = fallback.route_layer(from_fn(|_: Request, _: Next| async {
            StatusCode::NOT_FOUND.into_response()
        }));
    }
    fallback.layer(from_fn(|req: Request, next: Next| async move {
        let response = next.run(req).await;
        if !is_bare(&response, StatusCode::NOT_FOUND) {
            return response;
        }
        HttpError::not_found("no route matches the request").into_response()
    }))
}

/// Logs the `HttpError` behind every response that is still 5xx once filters have run. Sits
/// outside everything else (panics included), so each error is logged at most once.
pub(crate) fn log_server_errors(router: Router) -> Router {
    router.layer(from_fn(|req: Request, next: Next| async move {
        let response = next.run(req).await;
        if response.status().is_server_error()
            && let Some(ErrorExtension(err)) = response.extensions().get::<ErrorExtension>()
        {
            err.log();
        }
        response
    }))
}

pub(crate) fn with_middleware(router: Router, middleware: Arc<dyn ErasedMiddleware>) -> Router {
    router.layer(from_fn(move |req: Request, next: Next| {
        let middleware = middleware.clone();
        async move { middleware.handle(req, next).await }
    }))
}

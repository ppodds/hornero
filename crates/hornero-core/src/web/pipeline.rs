//! Turns guards, interceptors, filters and DI middleware into axum layers.
//!
//! Per route, from the outside in: filter → guards → interceptors → handler.
//! The layers use `route_layer`, so they only run for the methods a route registered: a request
//! that ends in the 405 fallback never reaches another route's guards, interceptors or filters.
//! Everything compiles down to `axum::middleware::from_fn`; there is no separate execution model.

use std::sync::Arc;

use axum::body::HttpBody;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use axum::{Extension, Router};

use super::extractor::RouteExtractors;
use super::filter::ErasedFilter;
use super::guard::{ErasedGuard, GuardContext};
use super::interceptor::ErasedInterceptor;
use super::middleware::ErasedMiddleware;
use super::route::RouteContext;
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

/// The innermost layer, so nothing outside it can drop the extractors from the request.
pub(crate) fn with_extractors(route: MethodRouter, extractors: RouteExtractors) -> MethodRouter {
    route.route_layer(Extension(extractors))
}

pub(crate) fn with_guards(
    route: MethodRouter,
    guards: Vec<Arc<dyn ErasedGuard>>,
    ctx: Arc<RouteContext>,
) -> MethodRouter {
    let guards: Arc<[Arc<dyn ErasedGuard>]> = guards.into();
    route.route_layer(from_fn(move |req: Request, next: Next| {
        let guards = guards.clone();
        let ctx = ctx.clone();
        async move { run_guards(&guards, &ctx, req, next).await }
    }))
}

async fn run_guards(
    guards: &[Arc<dyn ErasedGuard>],
    ctx: &RouteContext,
    req: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = req.into_parts();
    for guard in guards {
        let mut guard_ctx = GuardContext {
            route: ctx,
            parts: &mut parts,
        };
        if let Err(err) = guard.can_activate(&mut guard_ctx).await {
            return err.into_response();
        }
    }
    next.run(Request::from_parts(parts, body)).await
}

pub(crate) fn with_interceptor(
    route: MethodRouter,
    interceptor: Arc<dyn ErasedInterceptor>,
    ctx: Arc<RouteContext>,
) -> MethodRouter {
    route.route_layer(from_fn(move |req: Request, next: Next| {
        let interceptor = interceptor.clone();
        let ctx = ctx.clone();
        async move { interceptor.intercept(&ctx, req, next).await }
    }))
}

/// `filters` are ordered most specific first; the first one that catches the error handles it.
pub(crate) fn with_filters(
    route: MethodRouter,
    filters: Vec<Arc<dyn ErasedFilter>>,
    ctx: Arc<RouteContext>,
) -> MethodRouter {
    let filters: Arc<[Arc<dyn ErasedFilter>]> = filters.into();
    route.route_layer(from_fn(move |req: Request, next: Next| {
        let filters = filters.clone();
        let ctx = ctx.clone();
        async move {
            let response = next.run(req).await;
            let Some(ErrorExtension(err)) = response.extensions().get::<ErrorExtension>().cloned()
            else {
                return response;
            };
            let Some(filter) = filters.iter().find(|filter| filter.catches(&err)) else {
                return response;
            };
            let mut caught = filter.catch(&err, &ctx).await;
            // A filter that still answers 5xx without an error of its own: log the original.
            if caught.status().is_server_error()
                && caught.extensions().get::<ErrorExtension>().is_none()
            {
                caught.extensions_mut().insert(ErrorExtension(err));
            }
            caught
        }
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

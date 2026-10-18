//! Turns guards, interceptors, filters and DI middleware into axum layers.
//!
//! Per route, from the outside in: request scope → filter → guards → interceptors → handler.
//! The layers use `route_layer`, so they only run for the methods a route registered: a request
//! that ends in the 405 fallback never reaches another route's guards, interceptors or filters.
//! Everything compiles down to `axum::middleware::from_fn`; there is no separate execution model.

use std::sync::Arc;

use axum::Router;
use axum::body::HttpBody;
use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;

use super::filter::ErasedFilter;
use super::guard::{ErasedGuard, GuardContext};
use super::interceptor::ErasedInterceptor;
use super::middleware::ErasedMiddleware;
use super::route::RouteContext;
use crate::di::runtime::Runtime;
use crate::di::scope::{Handle, RequestHead, RequestScope, ScopeCell};
use crate::error::{ErrorExtension, HttpError};

fn scope_of(req: &Request) -> Option<Arc<RequestScope>> {
    req.extensions()
        .get::<ScopeCell>()
        .map(|cell| cell.0.clone())
}

/// Renders axum's bare 405 as problem+json.
///
/// axum adds the `Allow` header itself, after the layers, to whatever the fallback returns, so it
/// survives the replacement. This wraps the whole method router (fallback included), so it is the
/// one place that uses `layer` instead of `route_layer`.
pub(crate) fn with_problem_405(route: MethodRouter) -> MethodRouter {
    route.layer(from_fn(|req: Request, next: Next| async move {
        let response = next.run(req).await;
        // Only the bare fallback: a handler that answers 405 with a body or an error keeps it.
        let bare = response.status() == StatusCode::METHOD_NOT_ALLOWED
            && response.body().is_end_stream()
            && !response.headers().contains_key(header::CONTENT_TYPE)
            && response.extensions().get::<ErrorExtension>().is_none();
        if !bare {
            return response;
        }
        HttpError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed for this route",
        )
        .into_response()
    }))
}

pub(crate) fn with_scope(
    route: MethodRouter,
    rt: Arc<Runtime>,
    handler_needs_context: bool,
) -> MethodRouter {
    route.route_layer(from_fn(move |mut req: Request, next: Next| {
        let scope = RequestScope::new(rt.clone(), handler_needs_context);
        async move {
            req.extensions_mut().insert(ScopeCell(Arc::new(scope)));
            next.run(req).await
        }
    }))
}

pub(crate) fn with_guards(
    route: MethodRouter,
    guards: Vec<Handle<dyn ErasedGuard>>,
    ctx: Arc<RouteContext>,
) -> MethodRouter {
    let guards: Arc<[Handle<dyn ErasedGuard>]> = guards.into();
    route.route_layer(from_fn(move |req: Request, next: Next| {
        let guards = guards.clone();
        let ctx = ctx.clone();
        async move { run_guards(&guards, &ctx, req, next).await }
    }))
}

async fn run_guards(
    guards: &[Handle<dyn ErasedGuard>],
    ctx: &RouteContext,
    req: Request,
    next: Next,
) -> Response {
    let scope = scope_of(&req);
    let (mut parts, body) = req.into_parts();
    for handle in guards {
        handle.prime(scope.as_deref(), RequestHead::from_parts(&parts));
        let guard = match handle.resolve(scope.as_deref()).await {
            Ok(guard) => guard,
            Err(err) => return reject(scope.as_deref(), &parts, err),
        };
        let mut guard_ctx = GuardContext {
            route: ctx,
            parts: &mut parts,
        };
        let outcome = guard.can_activate(&mut guard_ctx).await;
        // The guard had mutable access to the request: later snapshots must see what it wrote.
        if let Some(scope) = scope.as_deref() {
            scope.invalidate_context();
        }
        if let Err(err) = outcome {
            return reject(scope.as_deref(), &parts, err);
        }
    }
    next.run(Request::from_parts(parts, body)).await
}

/// Answers a guard rejection. The request is consumed after this, so filters that need the
/// context get a snapshot of the request as the guards left it.
fn reject(scope: Option<&RequestScope>, parts: &Parts, err: HttpError) -> Response {
    if let Some(scope) = scope {
        scope.capture_for_filters(RequestHead::from_parts(parts));
    }
    err.into_response()
}

pub(crate) fn with_interceptor(
    route: MethodRouter,
    handle: Handle<dyn ErasedInterceptor>,
    ctx: Arc<RouteContext>,
) -> MethodRouter {
    route.route_layer(from_fn(move |req: Request, next: Next| {
        let handle = handle.clone();
        let ctx = ctx.clone();
        async move {
            let scope = scope_of(&req);
            handle.prime(scope.as_deref(), RequestHead::from_request(&req));
            let resolved = handle.resolve(scope.as_deref()).await;
            // The interceptor owns the request from here and may change it before `next.run`.
            if let Some(scope) = scope.as_deref() {
                scope.invalidate_context();
            }
            match resolved {
                Ok(interceptor) => interceptor.intercept(&ctx, req, next).await,
                Err(err) => err.into_response(),
            }
        }
    }))
}

/// `filters` are ordered most specific first; the first one that catches the error handles it.
pub(crate) fn with_filters(
    route: MethodRouter,
    filters: Vec<Handle<dyn ErasedFilter>>,
    ctx: Arc<RouteContext>,
) -> MethodRouter {
    let filters: Arc<[Handle<dyn ErasedFilter>]> = filters.into();
    route.route_layer(from_fn(move |req: Request, next: Next| {
        let filters = filters.clone();
        let ctx = ctx.clone();
        async move {
            let scope = scope_of(&req);
            // Filters run after the handler consumed the request. Whichever stage runs first takes
            // a snapshot for them; nothing is cloned here.
            if let Some(scope) = &scope
                && filters.iter().any(Handle::needs_context)
            {
                scope.keep_context_for_filters();
            }
            let response = next.run(req).await;
            let Some(ErrorExtension(err)) = response.extensions().get::<ErrorExtension>().cloned()
            else {
                return response;
            };
            for handle in filters.iter() {
                let filter = match handle.resolve(scope.as_deref()).await {
                    Ok(filter) => filter,
                    Err(resolve_err) => return resolve_err.into_response(),
                };
                if !filter.catches(&err) {
                    continue;
                }
                let mut caught = filter.catch(&err, &ctx).await;
                // A filter that still answers 5xx without an error of its own: log the original.
                if caught.status().is_server_error()
                    && caught.extensions().get::<ErrorExtension>().is_none()
                {
                    caught.extensions_mut().insert(ErrorExtension(err));
                }
                return caught;
            }
            response
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

pub(crate) fn with_middleware(router: Router, handle: Handle<dyn ErasedMiddleware>) -> Router {
    router.layer(from_fn(move |req: Request, next: Next| {
        let handle = handle.clone();
        async move {
            match handle.resolve(None).await {
                Ok(middleware) => middleware.handle(req, next).await,
                Err(err) => err.into_response(),
            }
        }
    }))
}

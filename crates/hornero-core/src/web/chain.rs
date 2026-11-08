//! A route's pipeline, run by hornero instead of being composed from axum layers.
//!
//! guards → interceptors → extractors → handler, each returning `Result`, so an error stays an
//! [`HttpError`] value from the stage that raises it to the exception filters. It becomes a
//! response once, at the end, through the renderer.

use std::sync::Arc;

use axum::extract::Request;
use axum::response::Response;
use axum::routing::{MethodFilter, MethodRouter, on};

use super::extractor::RouteExtractors;
use super::filter::ErasedFilter;
use super::guard::{ErasedGuard, GuardContext};
use super::interceptor::{CallHandler, ErasedInterceptor};
use super::route::RouteContext;
use crate::error::{ErrorExtension, HttpError};
use crate::util::BoxFuture;

/// A handler with its parameters extracted, as the innermost stage.
pub(crate) type Endpoint =
    Arc<dyn Fn(Request) -> BoxFuture<'static, Result<Response, HttpError>> + Send + Sync>;

/// Renders every error a route answers with: the application's error format.
pub type ErrorRenderer = fn(&HttpError, &RouteContext) -> Response;

pub(crate) struct RouteChain {
    pub(crate) ctx: RouteContext,
    pub(crate) guards: Vec<Arc<dyn ErasedGuard>>,
    pub(crate) interceptors: Vec<Arc<dyn ErasedInterceptor>>,
    /// Most specific first; the first one for the error's type maps it.
    pub(crate) filters: Vec<Arc<dyn ErasedFilter>>,
    pub(crate) renderer: Option<ErrorRenderer>,
    pub(crate) extractors: Option<RouteExtractors>,
    pub(crate) endpoint: Endpoint,
}

impl RouteChain {
    /// One axum handler per route: axum routes and answers 405, hornero runs the rest.
    pub(crate) fn into_method_router(self, filter: MethodFilter) -> MethodRouter {
        let chain = Arc::new(self);
        on(filter, move |req: Request| {
            let chain = chain.clone();
            async move { chain.call(req).await }
        })
    }

    async fn call(self: Arc<Self>, req: Request) -> Response {
        let result = match self.guard(req).await {
            Ok(req) => self.clone().run_from(0, req).await,
            Err(err) => Err(err),
        };
        match result {
            Ok(response) => response,
            Err(err) => self.answer(err).await,
        }
    }

    async fn guard(&self, req: Request) -> Result<Request, HttpError> {
        let (mut parts, body) = req.into_parts();
        for guard in &self.guards {
            let mut ctx = GuardContext {
                route: &self.ctx,
                parts: &mut parts,
            };
            guard.can_activate(&mut ctx).await?;
        }
        Ok(Request::from_parts(parts, body))
    }

    /// Runs the interceptors from `index` inwards, then the endpoint.
    pub(crate) fn run_from(
        self: Arc<Self>,
        index: usize,
        mut req: Request,
    ) -> BoxFuture<'static, Result<Response, HttpError>> {
        Box::pin(async move {
            let result = match self.interceptors.get(index) {
                Some(interceptor) => {
                    let next = CallHandler::new(self.clone(), index + 1);
                    interceptor.intercept(&self.ctx, req, next).await
                }
                None => {
                    if let Some(extractors) = &self.extractors {
                        req.extensions_mut().insert(extractors.clone());
                    }
                    (self.endpoint)(req).await
                }
            };
            // Mapped where it leaves a stage, so outer interceptors see the status the client gets.
            match result {
                Err(err) => Err(self.map(err).await),
                ok => ok,
            }
        })
    }

    /// A raised domain error mapped by the first filter for its type. The original error stays
    /// its source, so `err.is::<TodoError>()` still holds afterwards.
    async fn map(&self, err: HttpError) -> HttpError {
        if !err.is_raised() {
            return err;
        }
        let mut mapped = None;
        if let Some(source) = err.source() {
            for filter in &self.filters {
                if let Some(catch) = filter.try_catch(source, &self.ctx) {
                    mapped = Some(catch.await);
                    break;
                }
            }
        }
        match mapped {
            Some(mapped) => mapped.caused_by(err),
            None => err,
        }
    }

    /// Renders an error (a guard's is mapped here). The response keeps the error, for middleware
    /// and for logging 5xx.
    async fn answer(&self, err: HttpError) -> Response {
        let err = self.map(err).await;
        let mut response = match self.renderer {
            Some(render) => render(&err, &self.ctx),
            None => err.render(),
        };
        response.headers_mut().extend(err.headers().clone());
        response
            .extensions_mut()
            .insert(ErrorExtension(Arc::new(err)));
        response
    }
}

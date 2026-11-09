use std::future::Future;
use std::sync::Arc;

use axum::extract::Request;
use axum::response::Response;

use super::chain::RouteChain;
use super::route::RouteContext;
use crate::error::HttpError;
use crate::util::BoxFuture;

/// Wraps a handler with route awareness: logging, timing, caching, response envelopes.
///
/// Nest's RxJS-based interceptors become a plain `async fn(req, next) -> Result<Response, HttpError>`.
/// An error from inside (an inner interceptor, an extractor, the handler) arrives as the
/// `HttpError` itself, already mapped by exception filters, with the original domain error still
/// its source: `?` passes it on untouched and `map_err` replaces it. It becomes a response only at
/// the renderer, so changes to an `Ok` response do not reach error responses. Interceptors run
/// after guards; global ones wrap controller ones, which wrap handler ones.
pub trait Interceptor: Send + Sync + 'static {
    /// Handles the request; call `next.handle(req).await` to run the inner stages and the handler.
    fn intercept(
        &self,
        ctx: &RouteContext,
        req: Request,
        next: CallHandler,
    ) -> impl Future<Output = Result<Response, HttpError>> + Send;
}

/// The rest of the route: inner interceptors, extractors and the handler. Nest's `CallHandler`.
pub struct CallHandler {
    chain: Arc<RouteChain>,
    index: usize,
}

impl CallHandler {
    pub(crate) fn new(chain: Arc<RouteChain>, index: usize) -> Self {
        Self { chain, index }
    }

    /// Runs the inner interceptors, the extractors and the handler.
    pub fn handle(self, req: Request) -> impl Future<Output = Result<Response, HttpError>> + Send {
        self.chain.run_from(self.index, req)
    }
}

#[doc(hidden)]
pub trait ErasedInterceptor: Send + Sync {
    fn intercept<'a>(
        &'a self,
        ctx: &'a RouteContext,
        req: Request,
        next: CallHandler,
    ) -> BoxFuture<'a, Result<Response, HttpError>>;
}

impl<I: Interceptor> ErasedInterceptor for I {
    fn intercept<'a>(
        &'a self,
        ctx: &'a RouteContext,
        req: Request,
        next: CallHandler,
    ) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(Interceptor::intercept(self, ctx, req, next))
    }
}

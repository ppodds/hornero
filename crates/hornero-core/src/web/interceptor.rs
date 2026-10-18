use std::future::Future;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

use super::route::RouteContext;
use crate::util::BoxFuture;

/// Wraps a handler with route awareness: logging, timing, caching, response envelopes.
///
/// Nest's RxJS-based interceptors become a plain `async fn(req, next) -> Response`.
/// Interceptors run after guards; global ones wrap controller ones, which wrap handler ones.
pub trait Interceptor: Send + Sync + 'static {
    /// Handles the request; call `next.run(req).await` to run the inner layers and the handler.
    fn intercept(
        &self,
        ctx: &RouteContext,
        req: Request,
        next: Next,
    ) -> impl Future<Output = Response> + Send;
}

#[doc(hidden)]
pub trait ErasedInterceptor: Send + Sync {
    fn intercept<'a>(
        &'a self,
        ctx: &'a RouteContext,
        req: Request,
        next: Next,
    ) -> BoxFuture<'a, Response>;
}

impl<I: Interceptor> ErasedInterceptor for I {
    fn intercept<'a>(
        &'a self,
        ctx: &'a RouteContext,
        req: Request,
        next: Next,
    ) -> BoxFuture<'a, Response> {
        Box::pin(Interceptor::intercept(self, ctx, req, next))
    }
}

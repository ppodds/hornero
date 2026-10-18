use std::future::Future;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

use crate::util::BoxFuture;

/// A middleware that needs injected dependencies. Middleware without dependencies should just be
/// a tower layer (`m.layer(...)`, `Hornero::layer(...)`), which keeps the whole tower-http
/// ecosystem usable as-is.
///
/// Middleware runs before guards and is not route-aware. It must stay a singleton; the bootstrap
/// fails if it would become request-scoped.
pub trait Middleware: Send + Sync + 'static {
    /// Handles the request; call `next.run(req).await` to continue.
    fn handle(&self, req: Request, next: Next) -> impl Future<Output = Response> + Send;
}

#[doc(hidden)]
pub trait ErasedMiddleware: Send + Sync {
    fn handle(&self, req: Request, next: Next) -> BoxFuture<'_, Response>;
}

impl<M: Middleware> ErasedMiddleware for M {
    fn handle(&self, req: Request, next: Next) -> BoxFuture<'_, Response> {
        Box::pin(Middleware::handle(self, req, next))
    }
}

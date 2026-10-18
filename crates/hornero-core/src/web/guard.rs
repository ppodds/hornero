use std::future::Future;

use axum::http::request::Parts;

use super::route::RouteContext;
use crate::error::HttpError;
use crate::util::BoxFuture;

/// Everything a guard can see: the route and the request head.
///
/// Guards may write to `parts.extensions` (an authenticated user, say) for later extractors and
/// request-scoped providers to read.
pub struct GuardContext<'a> {
    /// The route being handled, with its metadata.
    pub route: &'a RouteContext,
    /// The request head. Data stored in `parts.extensions` is visible to everything that runs later.
    pub parts: &'a mut Parts,
}

/// Decides whether a request may reach its handler. Runs after middleware, before interceptors.
///
/// Returns `Result` rather than `bool`, so a guard picks the status (401 vs 403) and message.
/// When the handler itself needs the authenticated data, prefer a "proof" extractor
/// (`user: AuthUser`) over reading extensions by hand; use guards for declarative policies.
pub trait Guard: Send + Sync + 'static {
    /// `Ok(())` lets the request through; `Err` responds with that error instead.
    fn can_activate(
        &self,
        ctx: &mut GuardContext<'_>,
    ) -> impl Future<Output = Result<(), HttpError>> + Send;
}

#[doc(hidden)]
pub trait ErasedGuard: Send + Sync {
    fn can_activate<'a>(
        &'a self,
        ctx: &'a mut GuardContext<'_>,
    ) -> BoxFuture<'a, Result<(), HttpError>>;
}

impl<G: Guard> ErasedGuard for G {
    fn can_activate<'a>(
        &'a self,
        ctx: &'a mut GuardContext<'_>,
    ) -> BoxFuture<'a, Result<(), HttpError>> {
        Box::pin(Guard::can_activate(self, ctx))
    }
}

use std::future::Future;

use axum::response::Response;

use super::route::RouteContext;
use crate::error::HttpError;
use crate::util::BoxFuture;

/// Re-renders [`HttpError`] responses, Nest's exception filters.
///
/// Rust has no exceptions: handlers return `Result<_, HttpError>`, and filters see every
/// response that carries an `HttpError` (from handlers, guards, or failed request-scoped
/// construction). One filter handles each error: the most specific one that
/// [`catches`](ExceptionFilter::catches) it, looking at handler, then controller, then global
/// filters, each level in registration order. When none does, the error renders as is.
///
/// Domain errors raised with `?` arrive as 500s whose source is the original error, so a filter
/// that catches them by type turns them into proper responses:
///
/// ```ignore
/// impl ExceptionFilter for TodoErrorFilter {
///     fn catches(&self, err: &HttpError) -> bool {
///         err.is::<TodoError>()
///     }
///
///     async fn catch(&self, err: &HttpError, _: &RouteContext) -> Response {
///         match err.downcast_ref::<TodoError>() {
///             Some(TodoError::NotFound(id)) => HttpError::not_found(format!("todo {id} not found")).into_response(),
///             _ => err.render(),
///         }
///     }
/// }
/// ```
pub trait ExceptionFilter: Send + Sync + 'static {
    /// Whether this filter handles `err`, Nest's `@Catch(Type)`. Every error by default; narrow it
    /// with [`HttpError::is`] so other errors reach less specific filters.
    fn catches(&self, _err: &HttpError) -> bool {
        true
    }

    /// Renders the response for `err`. Start from [`HttpError::render`] to keep the defaults.
    fn catch(&self, err: &HttpError, ctx: &RouteContext) -> impl Future<Output = Response> + Send;
}

#[doc(hidden)]
pub trait ErasedFilter: Send + Sync {
    fn catches(&self, err: &HttpError) -> bool;
    fn catch<'a>(&'a self, err: &'a HttpError, ctx: &'a RouteContext) -> BoxFuture<'a, Response>;
}

impl<F: ExceptionFilter> ErasedFilter for F {
    fn catches(&self, err: &HttpError) -> bool {
        ExceptionFilter::catches(self, err)
    }

    fn catch<'a>(&'a self, err: &'a HttpError, ctx: &'a RouteContext) -> BoxFuture<'a, Response> {
        Box::pin(ExceptionFilter::catch(self, err, ctx))
    }
}

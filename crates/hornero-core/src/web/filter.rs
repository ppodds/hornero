use std::error::Error;
use std::future::Future;

use super::route::RouteContext;
use crate::error::HttpError;
use crate::util::BoxFuture;

/// Turns one domain error type into an [`HttpError`], Nest's `@Catch(TodoError)`.
///
/// A domain error enters the pipeline through `?` (or `HttpError::from`) in a guard, an
/// interceptor, an extractor or the handler. The most specific filter for its type maps it:
/// handler, then controller, then global filters, each level in registration order. When none
/// does, it stays a 500 whose source is the original error. An `HttpError` built with a status
/// (`HttpError::not_found`, say) is already an answer, and no filter sees it.
///
/// ```ignore
/// impl ExceptionFilter for TodoErrorFilter {
///     type Error = TodoError;
///
///     async fn catch(&self, err: &TodoError, _: &RouteContext) -> HttpError {
///         match err {
///             TodoError::NotFound(id) => HttpError::not_found(format!("todo {id} not found")),
///             TodoError::Storage(_) => HttpError::internal("internal server error"),
///         }
///     }
/// }
/// ```
pub trait ExceptionFilter: Send + Sync + 'static {
    /// The error this filter handles.
    type Error: Error + Send + Sync + 'static;

    /// The HTTP answer for `err`. The original error becomes its source, for the log and for
    /// `is` / `downcast_ref` downstream, replacing any source the answer sets: a failure of the
    /// filter's own (a lookup it needed, say) is for the filter to log. The answer is final, even
    /// one built with `HttpError::from`: no filter maps it again. Headers added around `err` (a
    /// `(headers, err)` tuple, say) stay on it.
    fn catch(
        &self,
        err: &Self::Error,
        ctx: &RouteContext,
    ) -> impl Future<Output = HttpError> + Send;
}

#[doc(hidden)]
pub trait ErasedFilter: Send + Sync {
    fn try_catch<'a>(
        &'a self,
        err: &'a (dyn Error + Send + Sync + 'static),
        ctx: &'a RouteContext,
    ) -> Option<BoxFuture<'a, HttpError>>;
}

impl<F: ExceptionFilter> ErasedFilter for F {
    fn try_catch<'a>(
        &'a self,
        err: &'a (dyn Error + Send + Sync + 'static),
        ctx: &'a RouteContext,
    ) -> Option<BoxFuture<'a, HttpError>> {
        let err = err.downcast_ref::<F::Error>()?;
        Some(Box::pin(ExceptionFilter::catch(self, err, ctx)))
    }
}

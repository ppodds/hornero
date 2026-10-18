use std::future::Future;
use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts};
use axum::response::IntoResponse;
use axum::routing::{MethodFilter, MethodRouter, on};

use crate::di::scope::{Handle, ScopeExt};

/// Closures usable as controller handlers: `Fn(Arc<C>, T1, ..., Tn) -> impl Future<Output: IntoResponse>`,
/// where `T1..Tn` are axum extractors (the last one may consume the body).
///
/// The controller instance is resolved per request through its [`Handle`]: a cloned `Arc` for
/// singletons, a request-scope lookup when the controller was pulled into request scope.
pub trait RouteHandler<C, Args>: Clone + Send + Sync + 'static {
    #[doc(hidden)]
    fn into_method_router(self, handle: Handle<C>, filter: MethodFilter) -> MethodRouter;
}

impl<C, F, Fut, Res> RouteHandler<C, ()> for F
where
    C: Send + Sync + 'static,
    F: Fn(Arc<C>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Res> + Send + 'static,
    Res: IntoResponse + 'static,
{
    fn into_method_router(self, handle: Handle<C>, filter: MethodFilter) -> MethodRouter {
        on(filter, move |scope: ScopeExt| async move {
            match handle.resolve_in(&scope).await {
                Ok(this) => (self)(this).await.into_response(),
                Err(err) => err.into_response(),
            }
        })
    }
}

macro_rules! impl_route_handler {
    ([$($ty:ident),*], $last:ident) => {
        #[allow(non_snake_case)]
        impl<C, F, Fut, Res, M, $($ty,)* $last> RouteHandler<C, (M, $($ty,)* $last,)> for F
        where
            C: Send + Sync + 'static,
            F: Fn(Arc<C>, $($ty,)* $last) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = Res> + Send + 'static,
            Res: IntoResponse + 'static,
            M: Send + 'static,
            $($ty: FromRequestParts<()> + Send + 'static,)*
            $last: FromRequest<(), M> + Send + 'static,
        {
            fn into_method_router(self, handle: Handle<C>, filter: MethodFilter) -> MethodRouter {
                on(filter, move |scope: ScopeExt, $($ty: $ty,)* $last: $last| async move {
                    match handle.resolve_in(&scope).await {
                        Ok(this) => (self)(this, $($ty,)* $last).await.into_response(),
                        Err(err) => err.into_response(),
                    }
                })
            }
        }
    };
}

impl_route_handler!([], T1);
impl_route_handler!([T1], T2);
impl_route_handler!([T1, T2], T3);
impl_route_handler!([T1, T2, T3], T4);
impl_route_handler!([T1, T2, T3, T4], T5);
impl_route_handler!([T1, T2, T3, T4, T5], T6);
impl_route_handler!([T1, T2, T3, T4, T5, T6], T7);
impl_route_handler!([T1, T2, T3, T4, T5, T6, T7], T8);
impl_route_handler!([T1, T2, T3, T4, T5, T6, T7, T8], T9);
impl_route_handler!([T1, T2, T3, T4, T5, T6, T7, T8, T9], T10);
impl_route_handler!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10], T11);
impl_route_handler!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11], T12);

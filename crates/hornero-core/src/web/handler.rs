use std::future::Future;
use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts};
use axum::response::IntoResponse;
use axum::routing::{MethodFilter, MethodRouter, on};

/// Closures usable as controller handlers: `Fn(Arc<C>, T1, ..., Tn) -> impl Future<Output: IntoResponse>`,
/// where `T1..Tn` are axum extractors (the last one may consume the body).
///
/// The controller is built during bootstrap; every request gets a clone of its `Arc`.
pub trait RouteHandler<C, Args>: Clone + Send + Sync + 'static {
    #[doc(hidden)]
    fn into_method_router(self, this: Arc<C>, filter: MethodFilter) -> MethodRouter;
}

impl<C, F, Fut, Res> RouteHandler<C, ()> for F
where
    C: Send + Sync + 'static,
    F: Fn(Arc<C>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Res> + Send + 'static,
    Res: IntoResponse + 'static,
{
    fn into_method_router(self, this: Arc<C>, filter: MethodFilter) -> MethodRouter {
        on(filter, move || (self)(this.clone()))
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
            fn into_method_router(self, this: Arc<C>, filter: MethodFilter) -> MethodRouter {
                on(filter, move |$($ty: $ty,)* $last: $last| (self)(this.clone(), $($ty,)* $last))
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

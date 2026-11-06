use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter, on};

use super::controller::RouteDef;

/// Closures usable as controller handlers: `Fn(Arc<C>, T1, ..., Tn) -> impl Future<Output: IntoResponse>`,
/// where `T1..Tn` are axum extractors or [`Extracted`](crate::Extracted) (the last one may consume
/// the body).
///
/// The controller is built during bootstrap; every request gets a clone of its `Arc`. Handlers run
/// with the state `()`, so an axum extractor that needs state does not qualify.
#[diagnostic::on_unimplemented(
    message = "this function cannot be a route handler",
    label = "a parameter cannot be extracted, or the return type is not `IntoResponse`",
    note = "a parameter is an axum extractor or `Extracted<E>`, and only the last one may consume the body",
    note = "handlers run with the state `()`: work that needs state or a provider goes in a `hornero::Extractor`, received as `Extracted<E>`"
)]
pub trait RouteHandler<C, Args>: Clone + Send + Sync + 'static {
    /// Registers what the parameters need on `route`.
    #[doc(hidden)]
    fn declare(route: &mut RouteDef);

    #[doc(hidden)]
    fn into_method_router(self, this: Arc<C>, filter: MethodFilter) -> MethodRouter;
}

/// A parameter axum extracts.
#[doc(hidden)]
pub enum ViaAxum {}

/// A parameter an [`Extractor`](crate::Extractor) builds.
#[doc(hidden)]
pub enum ViaExtractor {}

/// A last parameter that consumes the body.
#[doc(hidden)]
pub enum ViaBody {}

/// A last parameter that only needs the request head, extracted through `V`.
#[doc(hidden)]
pub struct ViaParts<V>(PhantomData<V>);

/// A handler parameter that leaves the body alone.
///
/// `Via` tells the implementations apart, as `M` does for axum's `FromRequest`: it lets one blanket
/// implementation cover axum's extractors and another cover [`Extracted`](crate::Extracted), so a
/// handler's signature alone says which providers its route needs, whoever wrote the route.
#[doc(hidden)]
pub trait HandlerParam<Via>: Sized {
    fn declare(_route: &mut RouteDef) {}

    fn extract(parts: &mut Parts) -> impl Future<Output = Result<Self, Response>> + Send;
}

impl<T: FromRequestParts<()>> HandlerParam<ViaAxum> for T {
    async fn extract(parts: &mut Parts) -> Result<Self, Response> {
        T::from_request_parts(parts, &())
            .await
            .map_err(IntoResponse::into_response)
    }
}

/// A handler's last parameter, which may consume the body.
#[doc(hidden)]
pub trait LastHandlerParam<Via>: Sized {
    fn declare(_route: &mut RouteDef) {}

    fn extract(request: Request) -> impl Future<Output = Result<Self, Response>> + Send;
}

impl<T: HandlerParam<V>, V> LastHandlerParam<ViaParts<V>> for T {
    fn declare(route: &mut RouteDef) {
        <T as HandlerParam<V>>::declare(route);
    }

    async fn extract(request: Request) -> Result<Self, Response> {
        let (mut parts, _) = request.into_parts();
        <T as HandlerParam<V>>::extract(&mut parts).await
    }
}

impl<T: FromRequest<()>> LastHandlerParam<ViaBody> for T {
    async fn extract(request: Request) -> Result<Self, Response> {
        T::from_request(request, &())
            .await
            .map_err(IntoResponse::into_response)
    }
}

/// Presents a [`HandlerParam`] to axum.
struct Param<T, V>(T, PhantomData<fn() -> V>);

impl<T: HandlerParam<V>, V> FromRequestParts<()> for Param<T, V> {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &()) -> Result<Self, Response> {
        Ok(Self(T::extract(parts).await?, PhantomData))
    }
}

/// Presents a [`LastHandlerParam`] to axum.
struct LastParam<T, V>(T, PhantomData<fn() -> V>);

impl<T: LastHandlerParam<V>, V> FromRequest<()> for LastParam<T, V> {
    type Rejection = Response;

    async fn from_request(request: Request, _: &()) -> Result<Self, Response> {
        Ok(Self(T::extract(request).await?, PhantomData))
    }
}

impl<C, F, Fut, Res> RouteHandler<C, ()> for F
where
    C: Send + Sync + 'static,
    F: Fn(Arc<C>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Res> + Send + 'static,
    Res: IntoResponse + 'static,
{
    fn declare(_route: &mut RouteDef) {}

    fn into_method_router(self, this: Arc<C>, filter: MethodFilter) -> MethodRouter {
        on(filter, move || (self)(this.clone()))
    }
}

macro_rules! impl_route_handler {
    ([$(($ty:ident, $via:ident)),*], ($last:ident, $last_via:ident)) => {
        #[allow(non_snake_case)]
        impl<C, F, Fut, Res, $($ty, $via,)* $last, $last_via>
            RouteHandler<C, ($($ty, $via,)* $last, $last_via)> for F
        where
            C: Send + Sync + 'static,
            F: Fn(Arc<C>, $($ty,)* $last) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = Res> + Send + 'static,
            Res: IntoResponse + 'static,
            $($ty: HandlerParam<$via> + Send + 'static, $via: 'static,)*
            $last: LastHandlerParam<$last_via> + Send + 'static,
            $last_via: 'static,
        {
            fn declare(route: &mut RouteDef) {
                $(<$ty as HandlerParam<$via>>::declare(route);)*
                <$last as LastHandlerParam<$last_via>>::declare(route);
            }

            fn into_method_router(self, this: Arc<C>, filter: MethodFilter) -> MethodRouter {
                on(filter, move |$($ty: Param<$ty, $via>,)* $last: LastParam<$last, $last_via>| {
                    (self)(this.clone(), $($ty.0,)* $last.0)
                })
            }
        }
    };
}

impl_route_handler!([], (T1, V1));
impl_route_handler!([(T1, V1)], (T2, V2));
impl_route_handler!([(T1, V1), (T2, V2)], (T3, V3));
impl_route_handler!([(T1, V1), (T2, V2), (T3, V3)], (T4, V4));
impl_route_handler!([(T1, V1), (T2, V2), (T3, V3), (T4, V4)], (T5, V5));
impl_route_handler!([(T1, V1), (T2, V2), (T3, V3), (T4, V4), (T5, V5)], (T6, V6));
impl_route_handler!(
    [(T1, V1), (T2, V2), (T3, V3), (T4, V4), (T5, V5), (T6, V6)],
    (T7, V7)
);
impl_route_handler!(
    [
        (T1, V1),
        (T2, V2),
        (T3, V3),
        (T4, V4),
        (T5, V5),
        (T6, V6),
        (T7, V7)
    ],
    (T8, V8)
);
impl_route_handler!(
    [
        (T1, V1),
        (T2, V2),
        (T3, V3),
        (T4, V4),
        (T5, V5),
        (T6, V6),
        (T7, V7),
        (T8, V8)
    ],
    (T9, V9)
);
impl_route_handler!(
    [
        (T1, V1),
        (T2, V2),
        (T3, V3),
        (T4, V4),
        (T5, V5),
        (T6, V6),
        (T7, V7),
        (T8, V8),
        (T9, V9)
    ],
    (T10, V10)
);
impl_route_handler!(
    [
        (T1, V1),
        (T2, V2),
        (T3, V3),
        (T4, V4),
        (T5, V5),
        (T6, V6),
        (T7, V7),
        (T8, V8),
        (T9, V9),
        (T10, V10)
    ],
    (T11, V11)
);
impl_route_handler!(
    [
        (T1, V1),
        (T2, V2),
        (T3, V3),
        (T4, V4),
        (T5, V5),
        (T6, V6),
        (T7, V7),
        (T8, V8),
        (T9, V9),
        (T10, V10),
        (T11, V11)
    ],
    (T12, V12)
);

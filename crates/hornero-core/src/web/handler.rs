use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::header;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use super::chain::Endpoint;
use super::controller::RouteDef;
use crate::error::HttpError;

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
    fn into_endpoint(self, this: Arc<C>) -> Endpoint;
}

/// Why a parameter could not be extracted.
#[doc(hidden)]
pub enum Rejection {
    /// An error, for exception filters and the renderer.
    Error(HttpError),
    /// An answer that is not an error (a redirect to a login page, say), sent as is.
    Answer(Response),
}

impl From<HttpError> for Rejection {
    fn from(err: HttpError) -> Self {
        Self::Error(err)
    }
}

impl Rejection {
    /// Reads an axum rejection: the `HttpError` it carries, else its status, text and headers.
    async fn read(mut response: Response) -> Self {
        if let Some(err) = HttpError::take_from(&mut response) {
            return Self::Error(err);
        }
        let status = response.status();
        if !(status.is_client_error() || status.is_server_error()) {
            return Self::Answer(response);
        }
        let (parts, body) = response.into_parts();
        let message = match axum::body::to_bytes(body, 64 * 1024).await {
            Ok(bytes) if !bytes.is_empty() => String::from_utf8_lossy(&bytes).into_owned(),
            _ => status
                .canonical_reason()
                .unwrap_or("invalid request")
                .to_owned(),
        };
        let mut err = HttpError::new(status, message);
        for (name, value) in &parts.headers {
            if name != header::CONTENT_TYPE && name != header::CONTENT_LENGTH {
                err = err.with_header(name.clone(), value.clone());
            }
        }
        Self::Error(err)
    }
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

    fn extract(parts: &mut Parts) -> impl Future<Output = Result<Self, Rejection>> + Send;
}

impl<T: FromRequestParts<()>> HandlerParam<ViaAxum> for T {
    async fn extract(parts: &mut Parts) -> Result<Self, Rejection> {
        // Turn the rejection into a `Response` before awaiting: `T::Rejection` need not be `Send`.
        let rejection = match T::from_request_parts(parts, &()).await {
            Ok(value) => return Ok(value),
            Err(rejection) => rejection.into_response(),
        };
        Err(Rejection::read(rejection).await)
    }
}

/// A handler's last parameter, which may consume the body.
#[doc(hidden)]
pub trait LastHandlerParam<Via>: Sized {
    fn declare(_route: &mut RouteDef) {}

    fn extract(request: Request) -> impl Future<Output = Result<Self, Rejection>> + Send;
}

impl<T: HandlerParam<V>, V> LastHandlerParam<ViaParts<V>> for T {
    fn declare(route: &mut RouteDef) {
        <T as HandlerParam<V>>::declare(route);
    }

    async fn extract(request: Request) -> Result<Self, Rejection> {
        let (mut parts, _) = request.into_parts();
        <T as HandlerParam<V>>::extract(&mut parts).await
    }
}

impl<T: FromRequest<()>> LastHandlerParam<ViaBody> for T {
    async fn extract(request: Request) -> Result<Self, Rejection> {
        let rejection = match T::from_request(request, &()).await {
            Ok(value) => return Ok(value),
            Err(rejection) => rejection.into_response(),
        };
        Err(Rejection::read(rejection).await)
    }
}

/// The handler's answer: an `HttpError` it returned stays an error, for filters and interceptors.
fn output(answer: impl IntoResponse) -> Result<Response, HttpError> {
    let mut response = answer.into_response();
    match HttpError::take_from(&mut response) {
        Some(err) => Err(err),
        None => Ok(response),
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

    fn into_endpoint(self, this: Arc<C>) -> Endpoint {
        Arc::new(move |_req: Request| {
            let handler = self.clone();
            let this = this.clone();
            Box::pin(async move { output(handler(this).await) })
        })
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

            fn into_endpoint(self, this: Arc<C>) -> Endpoint {
                Arc::new(move |req: Request| {
                    let handler = self.clone();
                    let this = this.clone();
                    Box::pin(async move {
                        let args = async {
                            #[allow(unused_mut)]
                            let (mut parts, body) = req.into_parts();
                            $(let $ty = <$ty as HandlerParam<$via>>::extract(&mut parts).await?;)*
                            let $last = <$last as LastHandlerParam<$last_via>>::extract(
                                Request::from_parts(parts, body),
                            )
                            .await?;
                            Ok::<_, Rejection>(($($ty,)* $last,))
                        }
                        .await;
                        match args {
                            Ok(($($ty,)* $last,)) => output(handler(this, $($ty,)* $last).await),
                            Err(Rejection::Error(err)) => Err(err),
                            Err(Rejection::Answer(response)) => Ok(response),
                        }
                    })
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

//! Cancels the requests still in flight when [`App::serve`](crate::App::serve) stops waiting for
//! them, so none keeps using a provider while its destroy hook runs.
//!
//! axum spawns a task per connection, which dropping the server future does not stop. `serve`
//! gives each connection a child of one token instead ([`Cancellable`]): a request only waits on
//! its own connection's token, so requests on different connections never contend for a lock.
//! Inside the router, [`answer_cancelled`] turns a cancelled request into a problem+json 503 that
//! still passes through the global layers; `Cancellable` cuts off response bodies still streaming
//! and catches what runs outside the router's layer, such as a global middleware stuck before
//! `next`.
//!
//! Tasks a handler spawns, WebSocket connections after their upgrade among them, are not
//! requests: they should watch [`Shutdown`](crate::Shutdown) themselves.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use http_body::{Frame, SizeHint};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tower::Service;

use crate::error::HttpError;

/// The token of the connection a request arrived on, for [`answer_cancelled`].
#[derive(Clone)]
struct ConnectionToken(Arc<CancellationToken>);

/// Marks the 503 of a cancelled request: its body is sent although the token is cancelled.
#[derive(Clone)]
struct ShuttingDown;

fn shutting_down() -> Response {
    let mut response = HttpError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "the server is shutting down",
    )
    .into_response();
    response.extensions_mut().insert(ShuttingDown);
    response
}

/// Answers a request with a 503 once its connection is cancelled. It sits inside the global layers
/// and middleware, so the 503 passes through them (CORS, request ids) and is logged like any other
/// 5xx. Requests that did not come through [`App::serve`](crate::App::serve) carry no token.
pub(crate) fn answer_cancelled(router: Router) -> Router {
    router.layer(from_fn(|req: Request, next: Next| async move {
        let Some(ConnectionToken(token)) = req.extensions().get::<ConnectionToken>().cloned()
        else {
            return next.run(req).await;
        };
        token
            .run_until_cancelled(next.run(req))
            .await
            .unwrap_or_else(shutting_down)
    }))
}

/// One connection's service, cancelled with the token [`App::serve`](crate::App::serve) gave it.
#[derive(Clone)]
pub(crate) struct Cancellable<S> {
    inner: S,
    token: Arc<CancellationToken>,
}

impl<S> Cancellable<S> {
    pub(crate) fn new(inner: S, token: CancellationToken) -> Self {
        Self {
            inner,
            token: Arc::new(token),
        }
    }
}

impl<S> Service<Request> for Cancellable<S>
where
    S: Service<Request, Response = Response, Error = Infallible>,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request) -> Self::Future {
        req.extensions_mut()
            .insert(ConnectionToken(self.token.clone()));
        let response = self.inner.call(req);
        let token = self.token.clone();
        Box::pin(async move {
            // `answer_cancelled` normally answers first, from inside the global layers.
            let response = match token.run_until_cancelled(response).await {
                Some(Ok(response)) => response,
                Some(Err(never)) => match never {},
                None => shutting_down(),
            };
            if response.extensions().get::<ShuttingDown>().is_some() {
                return Ok(response);
            }
            Ok(response.map(|body| CancellableBody::wrap(body, token)))
        })
    }
}

/// A response body that fails once its connection is cancelled, which makes hyper drop the
/// connection: an endless stream (server-sent events, say) would otherwise keep it open.
struct CancellableBody {
    inner: Body,
    token: Arc<CancellationToken>,
    /// Created the first time the body has nothing to send, so a body that is ready at once
    /// never registers with the token.
    cancelled: Option<Pin<Box<WaitForCancellationFutureOwned>>>,
}

impl CancellableBody {
    fn wrap(body: Body, token: Arc<CancellationToken>) -> Body {
        // An empty body has nothing to cut off.
        if body.is_end_stream() {
            return body;
        }
        Body::new(Self {
            inner: body,
            token,
            cancelled: None,
        })
    }
}

impl HttpBody for CancellableBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = &mut *self;
        // Checked before every frame, so a body that is always ready is cut off too.
        if !this.token.is_cancelled() {
            match Pin::new(&mut this.inner).poll_frame(cx) {
                Poll::Pending => {}
                ready => return ready,
            }
            let cancelled = this
                .cancelled
                .get_or_insert_with(|| Box::pin((*this.token).clone().cancelled_owned()));
            if cancelled.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }
        Poll::Ready(Some(Err(axum::Error::new(
            "the server shut down before the response body was sent",
        ))))
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

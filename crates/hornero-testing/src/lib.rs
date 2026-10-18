//! Testing utilities for hornero, the counterpart of `@nestjs/testing`.
//!
//! ```ignore
//! let app = TestApp::new(AppModule)
//!     .override_provider::<dyn TodoRepo>(Arc::new(InMemoryRepo::default()))
//!     .override_guard::<AuthGuard>(AllowAll)
//!     .build()
//!     .await?;
//!
//! let todo: Todo = app.client().get("/todos/1").await.assert_status(StatusCode::OK).json();
//! ```
//!
//! Requests go through the assembled router in-process (`tower::ServiceExt::oneshot`), with every
//! guard, interceptor, filter and middleware in place; no port is bound.

#![warn(missing_docs)]

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use bytes::Bytes;
use hornero::{
    App, BootError, ExceptionFilter, Guard, Hornero, Interceptor, Module, ProviderDef, ScopeReport,
    ShutdownError,
};
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tower::ServiceExt;

/// A bootstrapped application under test.
pub struct TestApp {
    app: App,
}

/// Configures a [`TestApp`]: overrides and any other [`Hornero`] setting.
pub struct TestAppBuilder {
    hornero: Hornero,
}

impl TestApp {
    /// Starts configuring a test application, like Nest's `Test.createTestingModule`.
    #[allow(
        clippy::new_ret_no_self,
        reason = "reads like `Test.createTestingModule(...).compile()`"
    )]
    pub fn new(root: impl Module) -> TestAppBuilder {
        TestAppBuilder {
            hornero: Hornero::new(root),
        }
    }

    /// Starts from an existing builder, e.g. the one your `main` uses.
    pub fn from_hornero(hornero: Hornero) -> TestAppBuilder {
        TestAppBuilder { hornero }
    }

    /// A new client without default headers.
    pub fn client(&self) -> TestClient {
        TestClient {
            router: self.app.router(),
            headers: HeaderMap::new(),
        }
    }

    /// The singleton registered under `T`. Panics with a helpful message when there is none.
    pub fn get<T: ?Sized + Send + Sync + 'static>(&self) -> Arc<T> {
        self.app.get::<T>().unwrap_or_else(|| {
            panic!(
                "no singleton `{}` is visible from the root module (request-scoped and transient providers cannot be fetched)",
                std::any::type_name::<T>()
            )
        })
    }

    /// Like [`get`](Self::get), returning `None` when there is no such singleton.
    pub fn try_get<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.app.get::<T>()
    }

    /// The singleton of `T` registered under the qualifier `Q`. Panics when there is none.
    pub fn get_qualified<T: ?Sized + Send + Sync + 'static, Q: 'static>(&self) -> Arc<T> {
        self.app.get_qualified::<T, Q>().unwrap_or_else(|| {
            panic!(
                "no singleton `{}` qualified by `{}` is visible from the root module (request-scoped and transient providers cannot be fetched)",
                std::any::type_name::<T>(),
                std::any::type_name::<Q>()
            )
        })
    }

    /// Every request-scoped component and why it is request-scoped.
    pub fn scope_report(&self) -> &ScopeReport {
        self.app.scope_report()
    }

    /// The underlying [`App`].
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The assembled router, for sending arbitrary requests with `tower::ServiceExt::oneshot`.
    pub fn router(&self) -> Router {
        self.app.router()
    }

    /// Runs the shutdown hooks.
    pub async fn close(self) -> Result<(), ShutdownError> {
        self.app.close().await
    }
}

impl TestAppBuilder {
    /// Replaces every provider of `T` with `value`, e.g.
    /// `override_provider::<dyn Repo>(Arc::new(InMemoryRepo::default()))`.
    pub fn override_provider<T: ?Sized + Send + Sync + 'static>(mut self, value: Arc<T>) -> Self {
        self.hornero = self.hornero.override_provider(value);
        self
    }

    /// Like [`override_provider`](Self::override_provider), taking the value itself.
    pub fn override_value<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.hornero = self.hornero.override_value(value);
        self
    }

    /// Replaces every provider of `def.key()`, e.g. with a fake that has dependencies of its own,
    /// or a qualified provider: `override_with(ProviderDef::value(fake).qualified::<Primary>())`.
    pub fn override_with(mut self, def: ProviderDef) -> Self {
        self.hornero = self.hornero.override_with(def);
        self
    }

    /// Replaces every use of the guard `G` with `guard`.
    pub fn override_guard<G: 'static>(mut self, guard: impl Guard) -> Self {
        self.hornero = self.hornero.override_guard::<G>(guard);
        self
    }

    /// Replaces every use of the interceptor `I` with `interceptor`.
    pub fn override_interceptor<I: 'static>(mut self, interceptor: impl Interceptor) -> Self {
        self.hornero = self.hornero.override_interceptor::<I>(interceptor);
        self
    }

    /// Replaces every use of the exception filter `F` with `filter`.
    pub fn override_filter<F: 'static>(mut self, filter: impl ExceptionFilter) -> Self {
        self.hornero = self.hornero.override_filter::<F>(filter);
        self
    }

    /// Any other builder setting: global enhancers, layers, a global prefix.
    pub fn configure(mut self, configure: impl FnOnce(Hornero) -> Hornero) -> Self {
        self.hornero = configure(self.hornero);
        self
    }

    /// Validates and boots the application.
    pub async fn build(self) -> Result<TestApp, BootError> {
        Ok(TestApp {
            app: self.hornero.build().await?,
        })
    }
}

/// An in-process HTTP client for the application's router.
#[derive(Clone)]
pub struct TestClient {
    router: Router,
    headers: HeaderMap,
}

impl TestClient {
    /// A header sent with every request from this client.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(header_name(name), header_value(value));
        self
    }

    /// Sends `authorization: Bearer {token}` with every request from this client.
    pub fn with_bearer(self, token: &str) -> Self {
        self.with_header(AUTHORIZATION.as_str(), &format!("Bearer {token}"))
    }

    /// A request with any method. `path` may include a query string.
    pub fn request(&self, method: Method, path: &str) -> TestRequest {
        let mut request = Request::new(Body::empty());
        *request.method_mut() = method;
        *request.uri_mut() = path
            .parse()
            .unwrap_or_else(|err| panic!("invalid request path `{path}`: {err}"));
        *request.headers_mut() = self.headers.clone();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
        TestRequest {
            router: self.router.clone(),
            request,
        }
    }

    /// A `GET` request.
    pub fn get(&self, path: &str) -> TestRequest {
        self.request(Method::GET, path)
    }

    /// A `POST` request.
    pub fn post(&self, path: &str) -> TestRequest {
        self.request(Method::POST, path)
    }

    /// A `PUT` request.
    pub fn put(&self, path: &str) -> TestRequest {
        self.request(Method::PUT, path)
    }

    /// A `PATCH` request.
    pub fn patch(&self, path: &str) -> TestRequest {
        self.request(Method::PATCH, path)
    }

    /// A `DELETE` request.
    pub fn delete(&self, path: &str) -> TestRequest {
        self.request(Method::DELETE, path)
    }
}

/// A request being built. `.await` it (or call [`send`](TestRequest::send)) to get a response.
pub struct TestRequest {
    router: Router,
    request: Request<Body>,
}

impl TestRequest {
    /// Sets a header, replacing the client's header of the same name.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.request
            .headers_mut()
            .insert(header_name(name), header_value(value));
        self
    }

    /// Sets `authorization: Bearer {token}`.
    pub fn bearer(self, token: &str) -> Self {
        self.header(AUTHORIZATION.as_str(), &format!("Bearer {token}"))
    }

    /// Sets the peer address handlers see through `ConnectInfo<SocketAddr>`; the default is
    /// `127.0.0.1:40000`.
    pub fn peer_addr(mut self, addr: SocketAddr) -> Self {
        self.request.extensions_mut().insert(ConnectInfo(addr));
        self
    }

    /// A JSON body, with `content-type: application/json`.
    pub fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Self {
        let bytes = serde_json::to_vec(body).expect("request body serializes to JSON");
        self.request
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        *self.request.body_mut() = Body::from(bytes);
        self
    }

    /// Any body. Does not set `content-type`.
    pub fn body(mut self, body: impl Into<Body>) -> Self {
        *self.request.body_mut() = body.into();
        self
    }

    /// Sends the request and buffers the response body.
    pub async fn send(self) -> TestResponse {
        let response = self
            .router
            .oneshot(self.request)
            .await
            .unwrap_or_else(|never| match never {});
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body can be read");
        TestResponse {
            status,
            headers,
            body,
        }
    }
}

impl IntoFuture for TestRequest {
    type Output = TestResponse;
    type IntoFuture = Pin<Box<dyn Future<Output = TestResponse> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.send())
    }
}

/// A buffered response.
#[derive(Debug)]
pub struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl TestResponse {
    /// The response status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// All response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// A header's value, or `None` when it is missing or not visible ASCII.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// The raw body.
    pub fn bytes(&self) -> &Bytes {
        &self.body
    }

    /// The body as text, with invalid UTF-8 replaced.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Deserializes the body, panicking with the body text if it does not match `T`.
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|err| {
            panic!(
                "response body is not a valid `{}`: {err}\nbody: {}",
                std::any::type_name::<T>(),
                self.text()
            )
        })
    }

    /// Panics with the response body when the status differs, then returns `self` for chaining.
    #[track_caller]
    pub fn assert_status(self, expected: StatusCode) -> Self {
        assert_eq!(
            self.status,
            expected,
            "unexpected status; body: {}",
            self.text()
        );
        self
    }
}

fn header_name(name: &str) -> HeaderName {
    HeaderName::try_from(name).unwrap_or_else(|err| panic!("invalid header name `{name}`: {err}"))
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::try_from(value)
        .unwrap_or_else(|err| panic!("invalid header value `{value}`: {err}"))
}

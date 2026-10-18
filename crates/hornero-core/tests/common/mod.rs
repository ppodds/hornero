#![allow(dead_code)]

pub mod domain;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use tower::ServiceExt;

/// Declares a struct whose tuple fields are injected `Arc`s, with a hand-written `Injectable`.
///
/// `service!(A: B, C)` declares `struct A(Arc<B>, Arc<C>)`; `service!(A; scope = Scope::Request)`
/// sets the declared scope.
#[macro_export]
macro_rules! service {
    ($name:ident $(: $($dep:ident),+)? $(; $($const_name:ident = $value:expr),+)?) => {
        #[allow(dead_code)]
        pub struct $name($($(pub ::std::sync::Arc<$dep>),+)?);

        impl ::hornero_core::Injectable for $name {
            $($(service!(@const $const_name $value);)+)?

            fn dependencies() -> Vec<::hornero_core::Key> {
                vec![$($(::hornero_core::Key::of::<$dep>()),+)?]
            }

            async fn construct(
                _resolver: &::hornero_core::Resolver,
            ) -> Result<Self, ::hornero_core::BoxError> {
                Ok($name($($(_resolver.get::<$dep>()?),+)?))
            }
        }
    };
    (@const scope $value:expr) => { const SCOPE: ::hornero_core::Scope = $value; };
    (@const pinned $value:expr) => { const SCOPE_PINNED: bool = $value; };
}

/// Declares a module from closures over `ModuleDef`.
#[macro_export]
macro_rules! module {
    ($name:ident, |$m:ident| $body:block) => {
        pub struct $name;

        impl ::hornero_core::Module for $name {
            fn define(&self, $m: &mut ::hornero_core::ModuleDef) {
                $body
            }
        }
    };
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl TestResponse {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|err| panic!("{err}: {}", self.body))
    }
}

pub async fn send(router: &Router, request: Request<Body>) -> TestResponse {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    TestResponse {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

pub async fn get(router: &Router, uri: &str) -> TestResponse {
    send(router, Request::get(uri).body(Body::empty()).unwrap()).await
}

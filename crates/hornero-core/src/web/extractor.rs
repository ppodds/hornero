//! Extractors that are providers.
//!
//! Handlers run with the state `()`, so an axum extractor only sees the request. An [`Extractor`]
//! is a provider as well: it declares dependencies, and a handler asks for it with [`Extracted`].
//! The parameter is the declaration, so the route gets a slot for it as `#[guards]` would give one,
//! and a dependency that cannot be resolved is a bootstrap error.

use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;

use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

use super::controller::RouteDef;
use super::enhancer::{EnhancerRef, extractor_slot_key};
use super::handler::{HandlerParam, ViaExtractor};
use crate::di::injectable::Injectable;
use crate::di::key::{Instance, Key};
use crate::error::HttpError;
use crate::util::BoxFuture;

/// Builds a handler parameter from the request, with its dependencies injected.
///
/// Use it when an axum extractor is not enough because the work needs a provider: a user loaded
/// from the database, a cookie jar that needs a key. It runs after guards and interceptors, only
/// for the handlers that ask for it with [`Extracted`], once per parameter.
///
/// ```ignore
/// #[derive(Injectable)]
/// struct CurrentUser {
///     users: Arc<Users>,
/// }
///
/// impl Extractor for CurrentUser {
///     type Output = User;
///
///     async fn extract(&self, parts: &mut Parts) -> Result<User, HttpError> {
///         let token = parts.headers.get("x-token").ok_or_else(|| HttpError::unauthorized("no token"))?;
///         self.users.by_token(token).await
///     }
/// }
///
/// #[get("/me")]
/// fn me(&self, Extracted(user): Extracted<CurrentUser>) -> String {
///     user.name
/// }
/// ```
///
/// The body stays with the handler, so a later parameter can still be `Json<T>`. An error is the
/// response, like a guard's: exception filters catch it.
pub trait Extractor: Send + Sync + 'static {
    /// What the handler receives.
    type Output: Send + 'static;

    /// Builds the value, or rejects the request.
    fn extract(
        &self,
        parts: &mut Parts,
    ) -> impl Future<Output = Result<Self::Output, HttpError>> + Send;
}

/// A handler parameter built by the [`Extractor`] `E`, which the route resolves in its controller's
/// module: a visible provider if there is one, otherwise a private one, as for a guard.
///
/// Unlike an axum extractor it only works in controller handlers, where the route can wire `E`.
/// It dereferences to the value, so `type User = Extracted<CurrentUser>` reads like the value too.
pub struct Extracted<E: Extractor>(pub E::Output);

impl<E: Extractor> Deref for Extracted<E> {
    type Target = E::Output;

    fn deref(&self) -> &E::Output {
        &self.0
    }
}

impl<E: Extractor + Injectable> HandlerParam<ViaExtractor> for Extracted<E> {
    fn declare(route: &mut RouteDef) {
        let slot = extractor_slot_key::<E>();
        if !route.extractors.iter().any(|e| e.slot_key == slot) {
            route.extractors.push(EnhancerRef::extractor::<E>());
        }
    }

    async fn extract(parts: &mut Parts) -> Result<Self, Response> {
        let extractor = parts
            .extensions
            .get::<RouteExtractors>()
            .expect("a route carries the extractors its handler declares")
            .get::<E::Output>(extractor_slot_key::<E>());
        match extractor.extract(parts).await {
            Ok(value) => Ok(Self(value)),
            Err(err) => Err(err.into_response()),
        }
    }
}

#[doc(hidden)]
pub trait ErasedExtractor<O>: Send + Sync {
    fn extract<'a>(&'a self, parts: &'a mut Parts) -> BoxFuture<'a, Result<O, HttpError>>;
}

impl<E: Extractor> ErasedExtractor<E::Output> for E {
    fn extract<'a>(&'a self, parts: &'a mut Parts) -> BoxFuture<'a, Result<E::Output, HttpError>> {
        Box::pin(Extractor::extract(self, parts))
    }
}

/// The extractor slots of one route. Handlers run with the state `()`, so these reach the
/// parameters through the request extensions.
#[derive(Clone)]
pub(crate) struct RouteExtractors(Arc<[(Key, Instance)]>);

impl RouteExtractors {
    pub(crate) fn new(slots: Vec<(Key, Instance)>) -> Self {
        Self(slots.into())
    }

    fn get<O: 'static>(&self, slot: Key) -> Arc<dyn ErasedExtractor<O>> {
        self.0
            .iter()
            .find(|(key, _)| *key == slot)
            .and_then(|(_, instance)| instance.downcast())
            .expect("a route holds a slot for each extractor its handler declares")
    }
}

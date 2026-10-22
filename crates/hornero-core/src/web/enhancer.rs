//! Guards, interceptors, filters and middleware ("enhancers") are providers too.
//!
//! Using `AuthGuard` on a controller creates a *slot* node in the controller's module. The slot
//! depends on `AuthGuard` (resolved with normal visibility rules, or created implicitly in that
//! module like Nest does) and stores it as `Arc<dyn ErasedGuard>`. Tests can override the slot
//! with any other `Guard` implementation without touching `AuthGuard`'s dependencies.

use std::any::type_name;
use std::marker::PhantomData;
use std::sync::Arc;

use super::filter::{ErasedFilter, ExceptionFilter};
use super::guard::{ErasedGuard, Guard};
use super::interceptor::{ErasedInterceptor, Interceptor};
use super::middleware::{ErasedMiddleware, Middleware};
use super::route::Metadata;
use super::routes::EnhancerInfo;
use crate::di::injectable::Injectable;
use crate::di::key::Key;
use crate::di::provider::ProviderDef;
use crate::util::short_type_name;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnhancerKind {
    Guard,
    Interceptor,
    Filter,
    Middleware,
}

impl EnhancerKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            EnhancerKind::Guard => "guard",
            EnhancerKind::Interceptor => "interceptor",
            EnhancerKind::Filter => "filter",
            EnhancerKind::Middleware => "middleware",
        }
    }
}

/// Registers the guard `G` for every route of the application, from inside a module: Nest's
/// `{ provide: APP_GUARD, useClass: G }`.
///
/// ```ignore
/// #[module(providers = [TokenStore, AppGuard<AuthGuard>])]
/// pub struct AuthModule;
/// ```
///
/// `G` is resolved in the declaring module, so it may depend on that module's private providers.
/// Module-declared global guards run after the ones registered with
/// [`Hornero::guard`](crate::Hornero::guard), imported modules before the modules importing them.
pub struct AppGuard<G>(PhantomData<fn() -> G>);

/// Registers the interceptor `I` around every route of the application, from inside a module:
/// Nest's `{ provide: APP_INTERCEPTOR, useClass: I }`. See [`AppGuard`] for resolution and order.
pub struct AppInterceptor<I>(PhantomData<fn() -> I>);

/// Registers the exception filter `F` for every route of the application, from inside a module:
/// Nest's `{ provide: APP_FILTER, useClass: F }`. Tried after route, controller and
/// [`Hornero::filter`](crate::Hornero::filter) filters. See [`AppGuard`] for resolution and order.
pub struct AppFilter<F>(PhantomData<fn() -> F>);

pub(crate) struct GuardSlot<G>(PhantomData<fn() -> G>);
pub(crate) struct InterceptorSlot<I>(PhantomData<fn() -> I>);
pub(crate) struct FilterSlot<F>(PhantomData<fn() -> F>);
pub(crate) struct MiddlewareSlot<M>(PhantomData<fn() -> M>);

/// The key of the slot `S` for the enhancer `E`, named after `E` in diagnostics (an unused
/// `override_guard::<E>()` reports `E`, not the slot type).
fn slot_key<S: 'static, E: 'static>() -> Key {
    Key::of::<S>().displayed_as::<E>()
}

/// A reference to an enhancer type, resolved into a slot node during bootstrap.
#[derive(Clone)]
pub(crate) struct EnhancerRef {
    pub(crate) kind: EnhancerKind,
    pub(crate) name: String,
    /// The module-level registration token (`AppGuard`), for diagnostics.
    token: &'static str,
    pub(crate) slot_key: Key,
    pub(crate) target_key: Key,
    pub(crate) make_slot: fn() -> ProviderDef,
    pub(crate) make_target: fn() -> ProviderDef,
    /// The enhancer type's [`Injectable::describe`], for the route table.
    describe: fn(&mut Metadata),
}

impl EnhancerRef {
    /// How the module-level registration is spelled, for diagnostics: `AppGuard<AuthGuard>`.
    pub(crate) fn token_name(&self) -> String {
        format!("{}<{}>", self.token, self.name)
    }

    /// The enhancer as the route table lists it.
    pub(crate) fn info(&self) -> EnhancerInfo {
        let mut meta = Metadata::default();
        (self.describe)(&mut meta);
        EnhancerInfo {
            name: self.name.clone(),
            type_id: self.target_key.type_id(),
            meta,
        }
    }

    pub(crate) fn guard<G: Guard + Injectable>() -> Self {
        Self {
            kind: EnhancerKind::Guard,
            name: short_type_name(type_name::<G>()),
            token: "AppGuard",
            slot_key: slot_key::<GuardSlot<G>, G>(),
            target_key: Key::of::<G>(),
            make_slot: || {
                ProviderDef::from_fn::<dyn ErasedGuard, _, _>(vec![Key::of::<G>()], |r| {
                    let guard = r.get::<G>();
                    async move { Ok(guard? as Arc<dyn ErasedGuard>) }
                })
                .with_key(slot_key::<GuardSlot<G>, G>())
            },
            make_target: ProviderDef::injectable::<G>,
            describe: <G as Injectable>::describe,
        }
    }

    pub(crate) fn interceptor<I: Interceptor + Injectable>() -> Self {
        Self {
            kind: EnhancerKind::Interceptor,
            name: short_type_name(type_name::<I>()),
            token: "AppInterceptor",
            slot_key: slot_key::<InterceptorSlot<I>, I>(),
            target_key: Key::of::<I>(),
            make_slot: || {
                ProviderDef::from_fn::<dyn ErasedInterceptor, _, _>(vec![Key::of::<I>()], |r| {
                    let interceptor = r.get::<I>();
                    async move { Ok(interceptor? as Arc<dyn ErasedInterceptor>) }
                })
                .with_key(slot_key::<InterceptorSlot<I>, I>())
            },
            make_target: ProviderDef::injectable::<I>,
            describe: <I as Injectable>::describe,
        }
    }

    pub(crate) fn filter<F: ExceptionFilter + Injectable>() -> Self {
        Self {
            kind: EnhancerKind::Filter,
            name: short_type_name(type_name::<F>()),
            token: "AppFilter",
            slot_key: slot_key::<FilterSlot<F>, F>(),
            target_key: Key::of::<F>(),
            make_slot: || {
                ProviderDef::from_fn::<dyn ErasedFilter, _, _>(vec![Key::of::<F>()], |r| {
                    let filter = r.get::<F>();
                    async move { Ok(filter? as Arc<dyn ErasedFilter>) }
                })
                .with_key(slot_key::<FilterSlot<F>, F>())
            },
            make_target: ProviderDef::injectable::<F>,
            describe: <F as Injectable>::describe,
        }
    }

    pub(crate) fn middleware<M: Middleware + Injectable>() -> Self {
        Self {
            kind: EnhancerKind::Middleware,
            name: short_type_name(type_name::<M>()),
            token: "Middleware",
            slot_key: slot_key::<MiddlewareSlot<M>, M>(),
            target_key: Key::of::<M>(),
            make_slot: || {
                ProviderDef::from_fn::<dyn ErasedMiddleware, _, _>(vec![Key::of::<M>()], |r| {
                    let middleware = r.get::<M>();
                    async move { Ok(middleware? as Arc<dyn ErasedMiddleware>) }
                })
                .with_key(slot_key::<MiddlewareSlot<M>, M>())
            },
            make_target: ProviderDef::injectable::<M>,
            describe: <M as Injectable>::describe,
        }
    }
}

/// A replacement for an enhancer slot, registered through `Hornero::override_guard` and friends.
pub(crate) fn guard_override<G: 'static>(guard: impl Guard) -> (Key, ProviderDef) {
    let key = slot_key::<GuardSlot<G>, G>();
    let erased: Arc<dyn ErasedGuard> = Arc::new(guard);
    (key, ProviderDef::from_arc(erased).with_key(key))
}

pub(crate) fn interceptor_override<I: 'static>(
    interceptor: impl Interceptor,
) -> (Key, ProviderDef) {
    let key = slot_key::<InterceptorSlot<I>, I>();
    let erased: Arc<dyn ErasedInterceptor> = Arc::new(interceptor);
    (key, ProviderDef::from_arc(erased).with_key(key))
}

pub(crate) fn filter_override<F: 'static>(filter: impl ExceptionFilter) -> (Key, ProviderDef) {
    let key = slot_key::<FilterSlot<F>, F>();
    let erased: Arc<dyn ErasedFilter> = Arc::new(filter);
    (key, ProviderDef::from_arc(erased).with_key(key))
}

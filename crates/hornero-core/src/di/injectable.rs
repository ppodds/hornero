use std::future::Future;
use std::sync::Arc;

use super::key::Key;
use super::resolver::Resolver;
use crate::lifecycle::{ErasedLifecycle, Lifecycle};
use crate::util::BoxError;
use crate::web::route::Metadata;

/// How long a provider instance lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// One instance for the whole application, built during bootstrap. The default.
    Singleton,
    /// One instance per HTTP request, built lazily the first time the request needs it.
    ///
    /// Request scope bubbles up like in Nest: anything that depends on a request-scoped
    /// provider becomes request-scoped too. The bootstrap logs a scope report listing every
    /// component that was pulled into request scope and the chain that caused it.
    Request,
    /// A fresh instance for every consumer.
    Transient,
}

/// Turns a [`Lifecycle`] implementation into the value stored in [`Injectable::LIFECYCLE`].
pub type LifecycleFn<T> = fn(&Arc<T>) -> Arc<dyn ErasedLifecycle>;

/// Opts a provider into lifecycle hooks: `const LIFECYCLE: Option<LifecycleFn<Self>> = hornero::hooks::<Self>();`
pub const fn hooks<T: Lifecycle>() -> Option<LifecycleFn<T>> {
    Some(erase_lifecycle::<T>)
}

fn erase_lifecycle<T: Lifecycle>(this: &Arc<T>) -> Arc<dyn ErasedLifecycle> {
    this.clone()
}

/// A type the container knows how to construct.
///
/// Usually derived: `#[derive(Injectable)]` treats every `Arc<T>` field as a dependency, and
/// `#[injectable]` on an `impl` block uses its `new` constructor. Implementing it by hand is the
/// macro-free (L1) way to use hornero.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not injectable",
    note = "add `#[derive(Injectable)]` to the struct, `#[injectable]` to an impl block with a `new` constructor, or implement `hornero::Injectable` by hand"
)]
pub trait Injectable: Send + Sync + Sized + 'static {
    /// Declared scope. The effective scope may still become [`Scope::Request`] through bubbling.
    const SCOPE: Scope = Scope::Singleton;

    /// When `true`, bubbling into request scope is a bootstrap error instead of a silent change.
    const SCOPE_PINNED: bool = false;

    /// Lifecycle hooks, see [`hooks`].
    const LIFECYCLE: Option<LifecycleFn<Self>> = None;

    /// The dependency keys, in the order [`construct`](Injectable::construct) resolves them.
    fn dependencies() -> Vec<Key>;

    /// Builds an instance from the dependencies in `resolver`. May be implemented as an `async fn`.
    fn construct(resolver: &Resolver) -> impl Future<Output = Result<Self, BoxError>> + Send;

    /// Metadata the type describes itself with when it is used as a guard, interceptor, filter
    /// or middleware, read back through [`EnhancerInfo::meta`](crate::EnhancerInfo::meta) in
    /// the route table. Nothing by default.
    ///
    /// It is how tooling learns what an enhancer does without constructing it: with hornero's
    /// `openapi` feature, `#[derive(Injectable)]` fills it in for a guard implementing
    /// `hornero_openapi::ApiSecurity`, and a hand-written impl calls
    /// `hornero_openapi::describe_security::<Self>(meta)`.
    fn describe(_meta: &mut Metadata) {}
}

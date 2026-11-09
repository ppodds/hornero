use std::future::Future;
use std::sync::Arc;

use super::key::Key;
use super::resolver::Resolver;
use crate::lifecycle::{ErasedLifecycle, Lifecycle};
use crate::util::BoxError;
use crate::web::route::Metadata;

/// How many instances of a provider exist. Every instance is built during bootstrap.
///
/// There is no request scope: request data reaches handlers through axum extractors, and from
/// there the services they call as arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// One instance for the whole application. The default.
    Singleton,
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
    /// The scope of this provider.
    const SCOPE: Scope = Scope::Singleton;

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

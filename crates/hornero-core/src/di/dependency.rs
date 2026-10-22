use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;

use super::key::Key;
use super::resolver::Resolver;
use crate::util::BoxError;

/// A way to request a provider: what a factory parameter, or [`Resolver::resolve`], accepts.
///
/// | Type | Key | Receives |
/// |---|---|---|
/// | `Arc<T>` | `T` | the provider of `T` |
/// | `Qualified<T, Q>` | `T` qualified by `Q` | the provider registered with [`ProviderDef::qualified::<Q>()`](crate::ProviderDef::qualified) |
/// | `Option<D>` | `D`'s key, [optional](Key::optional) | `None` when nothing provides it |
pub trait Dependency: Sized + 'static {
    /// The key this dependency resolves to.
    fn key() -> Key;

    /// Takes the dependency out of `resolver`, which holds the declared dependencies.
    fn resolve(resolver: &Resolver) -> Result<Self, BoxError>;
}

impl<T: ?Sized + Send + Sync + 'static> Dependency for Arc<T> {
    fn key() -> Key {
        Key::of::<T>()
    }

    fn resolve(resolver: &Resolver) -> Result<Self, BoxError> {
        resolver.require(Self::key())
    }
}

impl<D: Dependency> Dependency for Option<D> {
    fn key() -> Key {
        D::key().optional()
    }

    fn resolve(resolver: &Resolver) -> Result<Self, BoxError> {
        if resolver.is_absent(D::key()) {
            Ok(None)
        } else {
            D::resolve(resolver).map(Some)
        }
    }
}

/// The provider of `T` registered under the qualifier `Q`, Nest's `@Inject(TOKEN)`.
///
/// Qualifiers are marker types, so a typo is a compile error rather than a missing provider:
///
/// ```ignore
/// struct Primary;
/// struct Replica;
///
/// m.provider(ProviderDef::value(Pool::connect(PRIMARY_URL)).qualified::<Primary>())
///     .provider(ProviderDef::value(Pool::connect(REPLICA_URL)).qualified::<Replica>())
///     .factory(|primary: Qualified<Pool, Primary>, replica: Qualified<Pool, Replica>| async move {
///         Ok::<_, BoxError>(ReadWriteSplit::new(primary.into_inner(), replica.into_inner()))
///     });
/// ```
///
/// Derived and constructor-injected providers keep `Arc<T>` fields and parameters and mark them
/// with `#[inject(qualifier = Primary)]` instead. In `#[module(providers = [..], exports = [..])]`
/// and [`ModuleDef::provide`](crate::ModuleDef::provide), `Qualified<T, Q>` names the qualified
/// provider of an [`Injectable`](crate::Injectable) `T`.
pub struct Qualified<T: ?Sized, Q> {
    value: Arc<T>,
    _qualifier: PhantomData<fn() -> Q>,
}

impl<T: ?Sized, Q> Qualified<T, Q> {
    /// Wraps an instance.
    pub fn new(value: Arc<T>) -> Self {
        Self {
            value,
            _qualifier: PhantomData,
        }
    }

    /// The shared instance.
    pub fn into_inner(self) -> Arc<T> {
        self.value
    }
}

impl<T: ?Sized + Send + Sync + 'static, Q: 'static> Dependency for Qualified<T, Q> {
    fn key() -> Key {
        Key::of::<T>().qualified::<Q>()
    }

    fn resolve(resolver: &Resolver) -> Result<Self, BoxError> {
        resolver.require(Self::key()).map(Self::new)
    }
}

impl<T: ?Sized, Q> Deref for Qualified<T, Q> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: ?Sized, Q> Clone for Qualified<T, Q> {
    fn clone(&self) -> Self {
        Self::new(self.value.clone())
    }
}

impl<T: ?Sized, Q> From<Qualified<T, Q>> for Arc<T> {
    fn from(qualified: Qualified<T, Q>) -> Self {
        qualified.value
    }
}

impl<T: ?Sized + fmt::Debug, Q> fmt::Debug for Qualified<T, Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Primary;

    #[test]
    fn qualified_shares_one_instance() {
        let url: Qualified<str, Primary> = Qualified::new(Arc::from("postgres://primary"));
        assert!(url.starts_with("postgres"), "derefs to the value");
        assert_eq!(format!("{url:?}"), r#""postgres://primary""#);

        let copy = url.clone();
        let shared: Arc<str> = copy.into();
        assert!(Arc::ptr_eq(&shared, &url.into_inner()));
    }

    #[test]
    fn dependency_keys_carry_the_qualifier_and_optionality() {
        assert_eq!(<Arc<u8>>::key(), Key::of::<u8>());
        assert!(!<Arc<u8>>::key().is_optional());
        assert_eq!(
            <Qualified<u8, Primary>>::key(),
            Key::of::<u8>().qualified::<Primary>()
        );
        let optional = <Option<Qualified<u8, Primary>>>::key();
        assert!(optional.is_optional());
        assert_eq!(optional, Key::of::<u8>().qualified::<Primary>());
    }
}

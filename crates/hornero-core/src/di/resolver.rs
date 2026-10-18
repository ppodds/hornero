use std::sync::Arc;

use super::dependency::Dependency;
use super::key::{Instance, Key};
use crate::util::BoxError;

/// Hands a provider its already-constructed dependencies.
///
/// A resolver only contains the dependencies the provider declared, so a hand-written
/// [`Injectable`](crate::Injectable) that asks for an undeclared type gets an error instead
/// of silently reaching into the container.
pub struct Resolver {
    owner: Arc<str>,
    entries: Vec<(Key, Instance)>,
    /// Declared optional dependencies that nothing provides.
    absent: Vec<Key>,
}

impl Resolver {
    pub(crate) fn new(owner: Arc<str>, entries: Vec<(Key, Instance)>, absent: Vec<Key>) -> Self {
        Self {
            owner,
            entries,
            absent,
        }
    }

    /// Returns the dependency registered under `T`.
    pub fn get<T: ?Sized + Send + Sync + 'static>(&self) -> Result<Arc<T>, BoxError> {
        self.require(Key::of::<T>())
    }

    /// Returns any [`Dependency`]: `Arc<T>`, `Qualified<T, Q>`, or an `Option` of either for an
    /// optional dependency.
    ///
    /// ```ignore
    /// let primary: Qualified<Pool, Primary> = resolver.resolve()?;
    /// let metrics: Option<Arc<Metrics>> = resolver.resolve()?;
    /// ```
    pub fn resolve<D: Dependency>(&self) -> Result<D, BoxError> {
        D::resolve(self)
    }

    /// Human-readable name of the provider being constructed.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The instance registered under `key`, which must be a declared dependency that is provided.
    pub(crate) fn require<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: Key,
    ) -> Result<Arc<T>, BoxError> {
        let found = self
            .entries
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, instance)| instance.downcast::<T>());
        if let Some(value) = found {
            return Ok(value);
        }
        if self.is_absent(key) {
            return Err(format!(
                "{} requested `{key}`, an optional dependency that no module provides; resolve it as an `Option`",
                self.owner
            )
            .into());
        }
        Err(format!(
            "{} requested `{key}`, which is not one of its declared dependencies",
            self.owner
        )
        .into())
    }

    /// Whether `key` is a declared optional dependency that nothing provides.
    pub(crate) fn is_absent(&self, key: Key) -> bool {
        self.absent.contains(&key)
    }
}

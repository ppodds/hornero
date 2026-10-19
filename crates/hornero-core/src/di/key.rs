use std::any::{Any, TypeId, type_name};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::util::short_type_name;

/// Identifies a provider by type and, optionally, a qualifier type. Types are the injection tokens
/// in hornero: there are no string tokens.
///
/// `T` may be unsized, so `Key::of::<dyn UserRepo>()` is a valid key for a trait-object binding.
/// A qualifier is any marker type: `Key::of::<Pool>().qualified::<Primary>()` and
/// `Key::of::<Pool>().qualified::<Replica>()` are two providers of the same type.
///
/// In a dependency list, [`optional`](Key::optional) marks a dependency that may be missing. It
/// does not take part in comparisons: a key identifies a provider, not how it is requested.
#[derive(Clone, Copy)]
pub struct Key {
    id: TypeId,
    name: &'static str,
    qualifier: Option<Qualifier>,
    optional: bool,
}

#[derive(Clone, Copy)]
struct Qualifier {
    id: TypeId,
    name: &'static str,
}

impl Key {
    /// The key of `T`.
    pub fn of<T: ?Sized + 'static>() -> Self {
        Self {
            id: TypeId::of::<T>(),
            name: type_name::<T>(),
            qualifier: None,
            optional: false,
        }
    }

    /// The same type, qualified by the marker type `Q`.
    pub fn qualified<Q: 'static>(mut self) -> Self {
        self.qualifier = Some(Qualifier {
            id: TypeId::of::<Q>(),
            name: type_name::<Q>(),
        });
        self
    }

    /// The same key, named after `T` in diagnostics. Identity still comes from the keyed type, so
    /// an internal key such as an enhancer slot can be reported as the type the user wrote.
    pub(crate) fn displayed_as<T: ?Sized + 'static>(mut self) -> Self {
        self.name = type_name::<T>();
        self
    }

    /// Marks a dependency as optional: when nothing provides it, the consumer receives `None`
    /// instead of failing validation.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Whether this dependency is [`optional`](Key::optional).
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    /// The `TypeId` of the keyed type.
    pub fn type_id(&self) -> TypeId {
        self.id
    }

    /// The full `std::any::type_name` of the keyed type.
    pub fn type_name(&self) -> &'static str {
        self.name
    }

    /// The type name without module paths, used in diagnostics: `Pool`, or `Pool@Primary` when
    /// qualified.
    pub fn short_name(&self) -> String {
        match self.qualifier {
            Some(qualifier) => format!(
                "{}@{}",
                short_type_name(self.name),
                short_type_name(qualifier.name)
            ),
            None => short_type_name(self.name),
        }
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.qualifier.map(|q| q.id) == other.qualifier.map(|q| q.id)
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.qualifier.map(|q| q.id).hash(state);
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)?;
        if let Some(qualifier) = self.qualifier {
            write!(f, "@{}", qualifier.name)?;
        }
        if self.optional {
            f.write_str(" (optional)")?;
        }
        Ok(())
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.short_name())
    }
}

/// A type-erased provider instance. Always wraps an `Arc<T>` for the provider's key type `T`.
#[derive(Clone)]
pub(crate) struct Instance(Arc<dyn Any + Send + Sync>);

impl Instance {
    pub(crate) fn new<T: ?Sized + Send + Sync + 'static>(value: Arc<T>) -> Self {
        Self(Arc::new(value))
    }

    pub(crate) fn downcast<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.0.downcast_ref::<Arc<T>>().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Repo: Send + Sync {
        fn name(&self) -> &'static str;
    }
    struct Pg;
    impl Repo for Pg {
        fn name(&self) -> &'static str {
            "pg"
        }
    }

    #[test]
    fn instance_roundtrips_sized_and_unsized_types() {
        let sized = Instance::new(Arc::new(42u32));
        assert_eq!(*sized.downcast::<u32>().unwrap(), 42);
        assert!(sized.downcast::<u64>().is_none());

        let repo: Arc<dyn Repo> = Arc::new(Pg);
        let erased = Instance::new(repo);
        assert_eq!(erased.downcast::<dyn Repo>().unwrap().name(), "pg");
        assert!(erased.downcast::<Pg>().is_none());
    }

    #[test]
    fn keys_compare_by_type() {
        assert_eq!(Key::of::<dyn Repo>(), Key::of::<dyn Repo>());
        assert_ne!(Key::of::<dyn Repo>(), Key::of::<Pg>());
        assert_eq!(Key::of::<dyn Repo>().short_name(), "dyn Repo");
    }

    #[test]
    fn qualifiers_distinguish_keys_of_one_type() {
        struct Primary;
        struct Replica;
        let primary = Key::of::<Pg>().qualified::<Primary>();
        assert_eq!(primary, Key::of::<Pg>().qualified::<Primary>());
        assert_ne!(primary, Key::of::<Pg>());
        assert_ne!(primary, Key::of::<Pg>().qualified::<Replica>());
        assert_eq!(primary.short_name(), "Pg@Primary");
    }

    #[test]
    fn display_names_do_not_change_identity() {
        let slot = Key::of::<Pg>().displayed_as::<dyn Repo>();
        assert_eq!(slot, Key::of::<Pg>());
        assert_eq!(slot.short_name(), "dyn Repo");
        assert_eq!(
            Key::of::<Pg>()
                .qualified::<u8>()
                .displayed_as::<dyn Repo>()
                .short_name(),
            "dyn Repo@u8"
        );
    }

    #[test]
    fn optional_does_not_change_identity() {
        let optional = Key::of::<Pg>().optional();
        assert!(optional.is_optional());
        assert!(!Key::of::<Pg>().is_optional());
        assert_eq!(optional, Key::of::<Pg>());
        assert_eq!(optional.short_name(), "Pg");
    }
}

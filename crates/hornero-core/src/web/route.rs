use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use axum::http::Method;

/// Typed route metadata, hornero's replacement for Nest's `SetMetadata` + `Reflector`.
///
/// Values are keyed by type, so a guard asks for `ctx.route.meta::<Roles>()` instead of a string key.
#[derive(Clone, Default)]
pub struct Metadata {
    values: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl Metadata {
    /// Stores `value`, replacing any previous value of the same type.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        self.values.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// The value of type `T`, if any.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.values
            .get(&TypeId::of::<T>())
            .and_then(|value| value.downcast_ref::<T>())
    }

    /// Copies `other` over `self`; entries in `other` win.
    pub(crate) fn extend(&mut self, other: &Metadata) {
        for (key, value) in &other.values {
            self.values.insert(*key, value.clone());
        }
    }
}

/// Static information about the route being handled, available to guards, interceptors and filters.
pub struct RouteContext {
    pub(crate) controller: String,
    pub(crate) handler: &'static str,
    pub(crate) method: Method,
    pub(crate) path: String,
    pub(crate) meta: Metadata,
}

impl RouteContext {
    /// Short type name of the controller.
    pub fn controller(&self) -> &str {
        &self.controller
    }

    /// Name of the handler method.
    pub fn handler(&self) -> &'static str {
        self.handler
    }

    /// The HTTP method.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The full route path pattern, e.g. `/todos/{id}`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Route metadata of type `T`; handler-level values take precedence over controller-level ones.
    pub fn meta<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.meta.get::<T>()
    }
}

#[cfg(test)]
mod tests {
    use super::Metadata;

    #[derive(Debug, PartialEq)]
    struct Roles(&'static [&'static str]);
    struct Public;

    #[test]
    fn values_are_keyed_by_type_and_replaced_by_the_same_type() {
        let mut meta = Metadata::default();
        assert!(meta.get::<Roles>().is_none());
        meta.insert(Roles(&["user"]));
        meta.insert(Roles(&["admin"]));
        assert_eq!(meta.get::<Roles>(), Some(&Roles(&["admin"])));
        assert!(meta.get::<Public>().is_none());
    }

    #[test]
    fn extending_overrides_shared_types_and_keeps_the_rest() {
        let mut controller = Metadata::default();
        controller.insert(Roles(&["user"]));
        controller.insert(Public);
        let mut handler = Metadata::default();
        handler.insert(Roles(&["admin"]));

        controller.extend(&handler);
        assert_eq!(controller.get::<Roles>(), Some(&Roles(&["admin"])));
        assert!(controller.get::<Public>().is_some());
        assert!(handler.get::<Public>().is_none(), "the source is unchanged");
    }
}

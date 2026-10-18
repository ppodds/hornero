use std::any::TypeId;

use axum::http::Method;

use super::route::Metadata;

/// Every controller route of an application, modules in import order and routes in registration
/// order. hornero's counterpart of the route listing Nest's Swagger module builds from its metadata.
///
/// The table comes from the module graph alone, so getting it constructs nothing:
/// [`Hornero::routes`](crate::Hornero::routes) returns it without bootstrapping,
/// [`App::routes`](crate::App::routes) after, and providers and `mount` factories can inject it as
/// `Arc<Routes>`. Documentation generators such as `hornero-openapi` read it. Routes of mounted axum
/// routers are not listed: hornero does not know them.
#[derive(Clone, Default)]
pub struct Routes {
    pub(crate) prefix: String,
    pub(crate) routes: Vec<RouteInfo>,
}

impl Routes {
    /// The global prefix set with [`Hornero::global_prefix`](crate::Hornero::global_prefix), or `""`.
    /// Every [`RouteInfo::path`] already starts with it.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The routes.
    pub fn iter(&self) -> std::slice::Iter<'_, RouteInfo> {
        self.routes.iter()
    }

    /// The number of routes.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Whether the application has no controller routes.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

impl<'a> IntoIterator for &'a Routes {
    type Item = &'a RouteInfo;
    type IntoIter = std::slice::Iter<'a, RouteInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.iter()
    }
}

/// One controller route, as listed by [`Routes`].
#[derive(Clone)]
pub struct RouteInfo {
    pub(crate) method: Method,
    pub(crate) path: String,
    pub(crate) controller: String,
    pub(crate) handler: &'static str,
    pub(crate) module: String,
    pub(crate) meta: Metadata,
    pub(crate) guards: Vec<EnhancerInfo>,
}

impl RouteInfo {
    /// The HTTP method.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The full path pattern, global and controller prefixes included, e.g. `/api/todos/{id}`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Short type name of the controller.
    pub fn controller(&self) -> &str {
        &self.controller
    }

    /// Name of the handler method.
    pub fn handler(&self) -> &'static str {
        self.handler
    }

    /// Name of the module that registers the controller.
    pub fn module(&self) -> &str {
        &self.module
    }

    /// Route metadata of type `T`, the value a guard sees in
    /// [`RouteContext::meta`](crate::RouteContext::meta): handler-level values take precedence
    /// over controller-level ones.
    pub fn meta<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.meta.get::<T>()
    }

    /// The guards that run for this route, in the order they run: global ones (registered on
    /// `Hornero`, then declared by modules with `AppGuard`), then the controller's, then the
    /// handler's.
    pub fn guards(&self) -> &[EnhancerInfo] {
        &self.guards
    }
}

/// A guard (or other enhancer) as listed by [`RouteInfo::guards`]: its type, plus the metadata it
/// describes itself with, such as the security scheme `hornero-openapi` documents for it.
#[derive(Clone)]
pub struct EnhancerInfo {
    pub(crate) name: String,
    pub(crate) type_id: TypeId,
    pub(crate) meta: Metadata,
}

impl EnhancerInfo {
    /// Short type name of the enhancer.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `TypeId` of the enhancer type.
    pub fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Whether the enhancer is a `T`.
    pub fn is<T: 'static>(&self) -> bool {
        self.type_id == TypeId::of::<T>()
    }

    /// Metadata of type `T` the enhancer describes itself with.
    pub fn meta<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.meta.get::<T>()
    }
}

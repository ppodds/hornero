use std::any::{Any, TypeId, type_name};
use std::convert::Infallible;
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::response::IntoResponse;
use axum::routing::Route;
use tower::{Layer, Service};

use super::dependency::Qualified;
use super::injectable::Injectable;
use super::key::Key;
use super::provider::{FactoryFn, ProviderDef};
use crate::util::short_type_name;
use crate::web::controller::{Controller, ControllerDef, ControllerEntry};
use crate::web::enhancer::{AppFilter, AppGuard, AppInterceptor, EnhancerRef};
use crate::web::filter::ExceptionFilter;
use crate::web::guard::Guard;
use crate::web::interceptor::Interceptor;
use crate::web::middleware::Middleware;

/// A unit of encapsulation. Providers are private to their module unless exported.
///
/// Usually written with `#[module(...)]`. Implementing it by hand is the L1 API:
///
/// ```ignore
/// struct UsersModule;
///
/// impl Module for UsersModule {
///     fn define(&self, m: &mut ModuleDef) {
///         m.import(DatabaseModule)
///             .provide::<UserService>()
///             .controller::<UserController>()
///             .export::<UserService>();
///     }
/// }
/// ```
///
/// Dynamic modules (Nest's `forRoot`) are ordinary structs carrying configuration, which return
/// [`Identity::Instance`] from [`identity`](Module::identity) so that every call is a module of
/// its own. `define` may be called more than once (`validate()` and `build()` both call it), so it
/// must only describe the module; do I/O inside provider factories.
pub trait Module: Any + Send + Sync {
    /// Describes the module's imports, providers, controllers and exports.
    fn define(&self, m: &mut ModuleDef);

    /// How the imports of this module type relate; [`Identity::Auto`] by default.
    fn identity(&self) -> Identity {
        Identity::Auto
    }

    /// Name used in diagnostics and the scope report; the short type name by default.
    fn name(&self) -> String {
        short_type_name(type_name::<Self>())
    }
}

/// How the imports of one module type relate: which of them are one module, defined once.
///
/// Nest identifies a static module by its class and a dynamic module by reference, so every
/// `forRoot()` call is a module of its own. hornero follows the first rule for zero-sized modules
/// and asks a module that carries data to choose once it is imported more than once: merging the
/// imports would drop every value but the first, and splitting them would duplicate their
/// singletons and global enhancers, so neither is a safe guess.
///
/// To share one instance of a dynamic module, import it in one module and re-export it with
/// [`ModuleDef::export_module`], as Nest re-exports a module configured with `forRoot()`.
pub enum Identity {
    /// The default. A zero-sized module (no fields, or only `PhantomData`) is identified by its
    /// type, so importing it twice yields one instance. A module that carries data may be imported
    /// once; a second import of its type is a bootstrap error asking it to declare one of the
    /// other identities.
    Auto,
    /// Every import of the type is one module, defined by the first import; the values of the
    /// others are ignored.
    Type,
    /// Every import is a module of its own, like Nest's `forRoot()`. A module of this type cannot
    /// import its own type again, directly or through other modules: that would recurse forever,
    /// and is reported as a circular import. Use [`Identity::key`] to nest distinct values.
    Instance,
    /// Imports of the type with equal keys are one module, defined by the first of them; built
    /// with [`Identity::key`].
    Key(ModuleKey),
}

impl Identity {
    /// [`Identity::Key`]: imports of one module type are one module when their keys are equal
    /// (`==`). The key must cover every field that makes two imports differ, since only the first
    /// of the equal imports is defined.
    pub fn key<K: Eq + Send + Sync + 'static>(key: K) -> Self {
        Self::Key(ModuleKey(Box::new(key)))
    }
}

/// The key of an [`Identity::Key`], compared with its type's `==`.
pub struct ModuleKey(Box<dyn ErasedKey>);

impl PartialEq for ModuleKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_erased(other.0.as_ref())
    }
}

trait ErasedKey: Any + Send + Sync {
    fn eq_erased(&self, other: &dyn ErasedKey) -> bool;
}

impl<K: Eq + Send + Sync + 'static> ErasedKey for K {
    fn eq_erased(&self, other: &dyn ErasedKey) -> bool {
        (other as &dyn Any).downcast_ref::<K>() == Some(self)
    }
}

pub(crate) type LayerFn = Box<dyn FnOnce(Router) -> Router + Send>;

pub(crate) struct MountEntry {
    pub(crate) path: String,
    pub(crate) def: ProviderDef,
    /// The router itself for `mount_router`, so a dry run can assemble without running DI.
    pub(crate) router: Option<Router>,
}

/// Builder passed to [`Module::define`].
#[derive(Default)]
pub struct ModuleDef {
    pub(crate) global: bool,
    pub(crate) imports: Vec<Box<dyn Module>>,
    pub(crate) providers: Vec<ProviderDef>,
    pub(crate) controllers: Vec<ControllerEntry>,
    pub(crate) exports: Vec<Key>,
    /// Type and type name of each re-exported module.
    pub(crate) export_modules: Vec<(TypeId, &'static str)>,
    pub(crate) middlewares: Vec<EnhancerRef>,
    pub(crate) layers: Vec<LayerFn>,
    pub(crate) mounts: Vec<MountEntry>,
    /// Global enhancers declared here with `AppGuard<G>` and friends.
    pub(crate) app_enhancers: Vec<EnhancerRef>,
}

/// What [`ModuleDef::provide`] and `#[module(providers = [..])]` accept: any [`Injectable`] type,
/// registered as a class provider, [`Qualified<T, Q>`] for a class provider of `T` under the
/// qualifier `Q`, or a global-enhancer token such as [`AppGuard<G>`](crate::AppGuard).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not injectable",
    note = "add `#[derive(Injectable)]` to the struct, `#[injectable]` to an impl block with a `new` constructor, or implement `hornero::Injectable` by hand"
)]
pub trait Provide: 'static {
    /// Registers `Self` in `m`.
    fn register(m: &mut ModuleDef);
}

impl<T: Injectable> Provide for T {
    fn register(m: &mut ModuleDef) {
        m.provider(ProviderDef::injectable::<T>());
    }
}

impl<T: Injectable, Q: 'static> Provide for Qualified<T, Q> {
    fn register(m: &mut ModuleDef) {
        m.provider(ProviderDef::injectable::<T>().qualified::<Q>());
    }
}

impl<G: Guard + Injectable> Provide for AppGuard<G> {
    fn register(m: &mut ModuleDef) {
        m.app_enhancers.push(EnhancerRef::guard::<G>());
    }
}

impl<I: Interceptor + Injectable> Provide for AppInterceptor<I> {
    fn register(m: &mut ModuleDef) {
        m.app_enhancers.push(EnhancerRef::interceptor::<I>());
    }
}

impl<F: ExceptionFilter + Injectable> Provide for AppFilter<F> {
    fn register(m: &mut ModuleDef) {
        m.app_enhancers.push(EnhancerRef::filter::<F>());
    }
}

impl ModuleDef {
    /// Makes this module's exports visible to every module without importing it.
    pub fn global(&mut self) -> &mut Self {
        self.global = true;
        self
    }

    /// Imports a module, making its exports visible here.
    pub fn import(&mut self, module: impl Module) -> &mut Self {
        self.imports.push(Box::new(module));
        self
    }

    /// Registers `T` as a class provider (`Qualified<T, Q>` registers `T` under the qualifier
    /// `Q`), or, for `AppGuard<G>`, `AppInterceptor<I>` and `AppFilter<F>`, registers a global
    /// enhancer resolved in this module.
    pub fn provide<T: Provide>(&mut self) -> &mut Self {
        T::register(self);
        self
    }

    /// Registers a fixed value, Nest's `useValue`.
    pub fn provide_value<T: Send + Sync + 'static>(&mut self, value: T) -> &mut Self {
        self.provider(ProviderDef::value(value))
    }

    /// Registers a fixed `Arc`, which may be an `Arc<dyn Trait>`.
    pub fn provide_arc<T: ?Sized + Send + Sync + 'static>(&mut self, value: Arc<T>) -> &mut Self {
        self.provider(ProviderDef::from_arc(value))
    }

    /// Registers an async factory whose parameters are injected: `Arc<T>`, `Qualified<T, Q>`, or
    /// an `Option` of either.
    pub fn factory<Args, F: FactoryFn<Args>>(&mut self, f: F) -> &mut Self {
        self.provider(ProviderDef::factory(f))
    }

    /// Registers any provider definition, such as the result of [`bind!`](crate::bind).
    pub fn provider(&mut self, def: ProviderDef) -> &mut Self {
        self.providers.push(def);
        self
    }

    /// Registers a controller and its routes.
    pub fn controller<C: Controller>(&mut self) -> &mut Self {
        let mut def = ControllerDef::<C>::new();
        C::routes(&mut def);
        self.controllers.push(def.into_entry());
        self
    }

    /// Exports a provider: either one registered here, or one this module imports (a re-export).
    pub fn export<T: ?Sized + 'static>(&mut self) -> &mut Self {
        self.exports.push(Key::of::<T>());
        self
    }

    /// Exports the provider of `T` registered under the qualifier `Q`, like [`export`](Self::export).
    pub fn export_qualified<T: ?Sized + 'static, Q: 'static>(&mut self) -> &mut Self {
        self.exports.push(Key::of::<T>().qualified::<Q>());
        self
    }

    /// Re-exports everything the imported modules of type `M` export, whichever values they are,
    /// so `export_module::<SeaOrmModule>()` matches `SeaOrmModule::for_root(..)`.
    pub fn export_module<M: Module>(&mut self) -> &mut Self {
        self.export_modules
            .push((TypeId::of::<M>(), type_name::<M>()));
        self
    }

    /// Adds a DI-aware middleware that wraps this module's controllers.
    pub fn middleware<M: Middleware + Injectable>(&mut self) -> &mut Self {
        self.middlewares.push(EnhancerRef::middleware::<M>());
        self
    }

    /// Adds a plain tower layer that wraps this module's controllers.
    pub fn layer<L>(&mut self, layer: L) -> &mut Self
    where
        L: Layer<Route> + Clone + Send + Sync + 'static,
        L::Service: Service<Request> + Clone + Send + Sync + 'static,
        <L::Service as Service<Request>>::Response: IntoResponse + 'static,
        <L::Service as Service<Request>>::Error: Into<Infallible> + 'static,
        <L::Service as Service<Request>>::Future: Send + 'static,
    {
        self.layers
            .push(Box::new(move |router: Router| router.layer(layer)));
        self
    }

    /// Mounts a plain axum router (the L0 escape hatch).
    pub fn mount_router(&mut self, path: impl Into<String>, router: Router) -> &mut Self {
        self.mounts.push(MountEntry {
            path: path.into(),
            def: ProviderDef::value(router.clone()),
            router: Some(router),
        });
        self
    }

    /// Mounts an axum router built by a factory with injected dependencies.
    ///
    /// ```ignore
    /// m.mount("/legacy", |db: Arc<Db>| async move {
    ///     Ok::<_, BoxError>(legacy::router().with_state(db))
    /// });
    /// ```
    pub fn mount<Args, F>(&mut self, path: impl Into<String>, f: F) -> &mut Self
    where
        F: FactoryFn<Args, Output = Router>,
    {
        self.mounts.push(MountEntry {
            path: path.into(),
            def: ProviderDef::factory(f),
            router: None,
        });
        self
    }
}

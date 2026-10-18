use std::any::TypeId;
use std::future::Future;
use std::sync::Arc;

use super::dependency::Dependency;
use super::injectable::{Injectable, Scope};
use super::key::{Instance, Key};
use super::resolver::Resolver;
use crate::lifecycle::{ErasedLifecycle, Lifecycle};
use crate::util::{BoxError, BoxFuture};

/// What a factory produces: the instance plus its lifecycle hooks, captured while the concrete
/// type is still known (a `bind!` provider stores `Arc<dyn Trait>`, which cannot be downcast back).
pub(crate) struct Built {
    pub(crate) instance: Instance,
    pub(crate) lifecycle: Option<Arc<dyn ErasedLifecycle>>,
}

pub(crate) type Factory =
    Arc<dyn Fn(Resolver) -> BoxFuture<'static, Result<Built, BoxError>> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    None,
    RequestContext,
}

/// A type-erased provider registration: the key it satisfies, what it depends on, how to build it.
///
/// Every registration style (`provide::<T>()`, values, factories, `bind!`) produces one of these.
#[derive(Clone)]
pub struct ProviderDef {
    pub(crate) key: Key,
    pub(crate) deps: Vec<Key>,
    pub(crate) scope: Scope,
    pub(crate) pinned: bool,
    pub(crate) has_hooks: bool,
    pub(crate) factory: Factory,
    pub(crate) builtin: Builtin,
    /// A builder misuse recorded for the graph to report (builders cannot return errors).
    pub(crate) invalid: Option<String>,
}

impl ProviderDef {
    /// Nest's class provider: the container constructs `T` through [`Injectable`].
    pub fn injectable<T: Injectable>() -> Self {
        Self {
            key: Key::of::<T>(),
            deps: T::dependencies(),
            scope: T::SCOPE,
            pinned: T::SCOPE_PINNED,
            has_hooks: T::LIFECYCLE.is_some(),
            factory: Arc::new(|resolver| {
                Box::pin(async move {
                    let value = Arc::new(T::construct(&resolver).await?);
                    let lifecycle = T::LIFECYCLE.map(|erase| erase(&value));
                    Ok(Built {
                        instance: Instance::new(value),
                        lifecycle,
                    })
                })
            }),
            builtin: Builtin::None,
            invalid: None,
        }
    }

    /// Nest's `useClass` with an interface token: constructs `I` and stores it as `Arc<D>`.
    ///
    /// Stable Rust cannot unsize `Arc<I>` to `Arc<D>` generically, so the coercion is passed in.
    /// The [`bind!`](crate::bind) macro writes it for you.
    pub fn bind<D, I>(coerce: fn(Arc<I>) -> Arc<D>) -> Self
    where
        D: ?Sized + Send + Sync + 'static,
        I: Injectable,
    {
        Self {
            key: Key::of::<D>(),
            deps: I::dependencies(),
            scope: I::SCOPE,
            pinned: I::SCOPE_PINNED,
            has_hooks: I::LIFECYCLE.is_some(),
            factory: Arc::new(move |resolver| {
                Box::pin(async move {
                    let value = Arc::new(I::construct(&resolver).await?);
                    let lifecycle = I::LIFECYCLE.map(|erase| erase(&value));
                    Ok(Built {
                        instance: Instance::new(coerce(value)),
                        lifecycle,
                    })
                })
            }),
            builtin: Builtin::None,
            invalid: None,
        }
    }

    /// Nest's `useValue`.
    pub fn value<T: Send + Sync + 'static>(value: T) -> Self {
        Self::from_arc(Arc::new(value))
    }

    /// Like [`value`](ProviderDef::value), for unsized types such as `Arc<dyn Trait>`.
    pub fn from_arc<T: ?Sized + Send + Sync + 'static>(value: Arc<T>) -> Self {
        let instance = Instance::new(value);
        Self {
            key: Key::of::<T>(),
            deps: Vec::new(),
            scope: Scope::Singleton,
            pinned: false,
            has_hooks: false,
            factory: Arc::new(move |_| {
                let instance = instance.clone();
                Box::pin(async move {
                    Ok(Built {
                        instance,
                        lifecycle: None,
                    })
                })
            }),
            builtin: Builtin::None,
            invalid: None,
        }
    }

    /// Nest's `useFactory`: an async closure whose parameters are injected. Each parameter is a
    /// [`Dependency`](crate::Dependency): `Arc<T>`, `Qualified<T, Q>`, or an `Option` of either.
    ///
    /// ```ignore
    /// ProviderDef::factory(|cfg: Arc<AppConfig>| async move {
    ///     Ok::<_, BoxError>(Mailer::connect(&cfg.smtp_url).await?)
    /// })
    /// ```
    pub fn factory<Args, F: FactoryFn<Args>>(f: F) -> Self {
        let f = Arc::new(f);
        Self::from_fn::<F::Output, _, _>(F::dependencies(), move |resolver| {
            let fut = f.call(&resolver);
            async move { fut.await.map(Arc::new) }
        })
    }

    /// The lowest-level constructor: explicit dependency keys and a closure over the [`Resolver`].
    pub fn from_fn<T, F, Fut>(deps: Vec<Key>, f: F) -> Self
    where
        T: ?Sized + Send + Sync + 'static,
        F: Fn(Resolver) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Arc<T>, BoxError>> + Send + 'static,
    {
        Self {
            key: Key::of::<T>(),
            deps,
            scope: Scope::Singleton,
            pinned: false,
            has_hooks: false,
            factory: Arc::new(move |resolver| {
                let fut = f(resolver);
                Box::pin(async move {
                    Ok(Built {
                        instance: Instance::new(fut.await?),
                        lifecycle: None,
                    })
                })
            }),
            builtin: Builtin::None,
            invalid: None,
        }
    }

    pub(crate) fn request_context() -> Self {
        Self {
            key: Key::of::<crate::RequestContext>(),
            deps: Vec::new(),
            scope: Scope::Request,
            pinned: false,
            has_hooks: false,
            factory: Arc::new(|_| {
                Box::pin(async { Err("RequestContext is provided by the request scope".into()) })
            }),
            builtin: Builtin::RequestContext,
            invalid: None,
        }
    }

    /// Sets the declared scope.
    pub fn scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// Makes bubbling into request scope a bootstrap error for this provider.
    pub fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    /// Registers the provider under the qualifier `Q`, so a module can hold several providers of
    /// one type. Consumers ask for it with `#[inject(qualifier = Q)]` or
    /// [`Qualified<T, Q>`](crate::Qualified).
    ///
    /// ```ignore
    /// m.provider(ProviderDef::value(Pool::connect(PRIMARY_URL)).qualified::<Primary>())
    ///     .export_qualified::<Pool, Primary>();
    /// ```
    pub fn qualified<Q: 'static>(mut self) -> Self {
        self.key = self.key.qualified::<Q>();
        self
    }

    /// Runs `T`'s lifecycle hooks for a factory or value provider of type `T`.
    pub fn with_hooks<T: Lifecycle>(mut self) -> Self {
        // Compare types, not keys: a qualified provider of `T` takes hooks of `T` too.
        if self.key.type_id() != TypeId::of::<T>() {
            self.invalid = Some(format!(
                "`with_hooks::<{}>()` was called on a provider of `{}`",
                Key::of::<T>(),
                self.key
            ));
            return self;
        }
        let inner = self.factory;
        self.factory = Arc::new(move |resolver| {
            let fut = inner(resolver);
            Box::pin(async move {
                let mut built = fut.await?;
                if let Some(value) = built.instance.downcast::<T>() {
                    built.lifecycle = Some(value as Arc<dyn ErasedLifecycle>);
                }
                Ok(built)
            })
        });
        self.has_hooks = true;
        self
    }

    /// Replaces the key while keeping the instance type. Used for enhancer slots.
    pub(crate) fn with_key(mut self, key: Key) -> Self {
        self.key = key;
        self
    }

    /// The key this provider satisfies.
    pub fn key(&self) -> Key {
        self.key
    }

    /// The keys this provider depends on.
    pub fn dependencies(&self) -> &[Key] {
        &self.deps
    }
}

/// Closures usable as factories: `Fn(Arc<A>, Arc<B>, ...) -> impl Future<Output = Result<T, E>>`.
///
/// Every parameter must be a [`Dependency`]: `Arc<T>`, `Qualified<T, Q>`, or an `Option` of
/// either. Each one's key becomes a dependency.
pub trait FactoryFn<Args>: Send + Sync + 'static {
    /// The type the factory produces.
    type Output: Send + Sync + 'static;

    /// The key of every parameter type.
    fn dependencies() -> Vec<Key>;

    /// Resolves the arguments and starts the factory.
    fn call(&self, resolver: &Resolver) -> BoxFuture<'static, Result<Self::Output, BoxError>>;
}

macro_rules! impl_factory_fn {
    ($($arg:ident),*) => {
        impl<F, Fut, T, E, $($arg,)*> FactoryFn<($($arg,)*)> for F
        where
            F: Fn($($arg),*) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Result<T, E>> + Send + 'static,
            T: Send + Sync + 'static,
            E: Into<BoxError>,
            $($arg: Dependency,)*
        {
            type Output = T;

            fn dependencies() -> Vec<Key> {
                vec![$(<$arg as Dependency>::key()),*]
            }

            #[allow(non_snake_case, unused_variables)]
            fn call(&self, resolver: &Resolver) -> BoxFuture<'static, Result<T, BoxError>> {
                $(
                    let $arg = match <$arg as Dependency>::resolve(resolver) {
                        Ok(value) => value,
                        Err(err) => return Box::pin(async move { Err(err) }),
                    };
                )*
                let fut = (self)($($arg),*);
                Box::pin(async move { fut.await.map_err(Into::into) })
            }
        }
    };
}

impl_factory_fn!();
impl_factory_fn!(A1);
impl_factory_fn!(A1, A2);
impl_factory_fn!(A1, A2, A3);
impl_factory_fn!(A1, A2, A3, A4);
impl_factory_fn!(A1, A2, A3, A4, A5);
impl_factory_fn!(A1, A2, A3, A4, A5, A6);
impl_factory_fn!(A1, A2, A3, A4, A5, A6, A7);
impl_factory_fn!(A1, A2, A3, A4, A5, A6, A7, A8);

/// Binds a trait object to an implementation, Nest's `{ provide: TOKEN, useClass: Impl }`.
///
/// ```ignore
/// m.provider(bind!(dyn UserRepo => PgUserRepo));
/// ```
#[macro_export]
macro_rules! bind {
    ($target:ty => $implementation:ty) => {
        $crate::ProviderDef::bind::<$target, $implementation>(
            |value: ::std::sync::Arc<$implementation>| -> ::std::sync::Arc<$target> { value },
        )
    };
}

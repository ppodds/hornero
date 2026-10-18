//! SeaORM integration for hornero.
//!
//! ```ignore
//! #[module(imports = [
//!     ConfigModule::for_root(source).load::<AppConfig>(),
//!     SeaOrmModule::for_root_with(|config: Arc<AppConfig>| async move {
//!         Ok::<_, BoxError>(ConnectOptions::new(&config.database_url))
//!     }),
//!     TodoModule,
//! ])]
//! struct AppModule;
//!
//! #[derive(Injectable)]
//! struct TodoRepo { db: Arc<Db> }   // `Db` derefs to `sea_orm::DatabaseConnection`
//! ```
//!
//! The connection is opened while the application boots (an unreachable database is a bootstrap
//! error) and closed in `on_application_shutdown`. The module is global by default.
//!
//! There is no `forFeature([Entity])`: SeaORM has no per-entity repository object, so feature
//! modules inject `Arc<Db>` and use `Entity::find()` directly.
//!
//! Enable the `sqlite` feature, or any driver feature on your own `sea-orm` dependency
//! (`sqlx-postgres`, `sqlx-mysql`); Cargo unifies the features.

#![warn(missing_docs)]

use std::any::type_name;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hornero::{
    BoxError, BoxFuture, FactoryFn, Key, Lifecycle, Module, ModuleDef, ModuleId, ProviderDef,
    Resolver,
};
pub use sea_orm;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};

/// Tag of the default connection.
pub struct Primary;

/// A database connection, tagged so an application can hold several (`Db<Replica>`).
pub struct Db<Tag = Primary> {
    conn: DatabaseConnection,
    _tag: PhantomData<fn() -> Tag>,
}

impl<Tag> Db<Tag> {
    /// Wraps an existing connection, e.g. to override `Db` in tests.
    pub fn new(conn: DatabaseConnection) -> Self {
        Self {
            conn,
            _tag: PhantomData,
        }
    }

    /// The underlying connection; `Db` also derefs to it.
    pub fn conn(&self) -> &DatabaseConnection {
        &self.conn
    }
}

impl<Tag> Deref for Db<Tag> {
    type Target = DatabaseConnection;

    fn deref(&self) -> &DatabaseConnection {
        &self.conn
    }
}

impl<Tag: 'static> Lifecycle for Db<Tag> {
    async fn on_application_shutdown(&self) -> Result<(), BoxError> {
        tracing::debug!(
            connection = type_name::<Tag>(),
            "closing database connection"
        );
        self.conn.close_by_ref().await?;
        Ok(())
    }
}

type OptionsFactory =
    Arc<dyn Fn(&Resolver) -> BoxFuture<'static, Result<ConnectOptions, BoxError>> + Send + Sync>;

/// How to obtain the connect options: fixed options are a factory without dependencies.
#[derive(Clone)]
struct Options {
    deps: Vec<Key>,
    factory: OptionsFactory,
}

/// Provides and exports `Arc<Db<Tag>>`.
///
/// Like Nest's `TypeOrmModule.forRoot`, register it once, in the application's root module; other
/// modules just inject `Arc<Db>`. Every instance has its own identity, so two roots for the same
/// tag make `Db<Tag>` ambiguous instead of silently dropping one of them.
pub struct SeaOrmModule<Tag = Primary> {
    instance: u64,
    options: Options,
    global: bool,
    _tag: PhantomData<fn() -> Tag>,
}

/// Source of the per-instance module identity.
static INSTANCES: AtomicU64 = AtomicU64::new(1);

fn next_instance() -> u64 {
    INSTANCES.fetch_add(1, Ordering::Relaxed)
}

impl SeaOrmModule {
    /// Connects with fixed options.
    pub fn for_root(options: impl Into<ConnectOptions>) -> Self {
        let options: ConnectOptions = options.into();
        Self {
            instance: next_instance(),
            options: Options {
                deps: Vec::new(),
                factory: Arc::new(move |_| {
                    let options = options.clone();
                    Box::pin(async move { Ok(options) })
                }),
            },
            global: true,
            _tag: PhantomData,
        }
    }

    /// Connects with options computed from injected providers, Nest's `forRootAsync`.
    pub fn for_root_with<Args, F>(factory: F) -> Self
    where
        F: FactoryFn<Args, Output = ConnectOptions>,
    {
        let factory = Arc::new(factory);
        Self {
            instance: next_instance(),
            options: Options {
                deps: F::dependencies(),
                factory: Arc::new(move |resolver| factory.call(resolver)),
            },
            global: true,
            _tag: PhantomData,
        }
    }
}

impl<Tag> SeaOrmModule<Tag> {
    /// Re-tags the connection: `SeaOrmModule::for_root(url).tag::<Replica>()` provides `Db<Replica>`.
    pub fn tag<Other>(self) -> SeaOrmModule<Other> {
        SeaOrmModule {
            instance: self.instance,
            options: self.options,
            global: self.global,
            _tag: PhantomData,
        }
    }

    /// Makes the module non-global: only modules importing it see the connection.
    pub fn local(mut self) -> Self {
        self.global = false;
        self
    }
}

impl<Tag: Send + Sync + 'static> Module for SeaOrmModule<Tag> {
    fn id(&self) -> ModuleId {
        ModuleId::of::<Self>().with_discriminator(self.instance)
    }

    fn define(&self, m: &mut ModuleDef) {
        if self.global {
            m.global();
        }
        let Options { deps, factory } = self.options.clone();
        m.provider(
            ProviderDef::from_fn::<Db<Tag>, _, _>(deps, move |resolver| {
                let options = factory(&resolver);
                async move {
                    let conn = Database::connect(options.await?).await?;
                    Ok(Arc::new(Db::<Tag>::new(conn)))
                }
            })
            .with_hooks::<Db<Tag>>(),
        )
        .export::<Db<Tag>>();
    }
}

mod common;

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hornero_core::{
    BoxError, Hornero, Identity, Injectable, Key, Lifecycle, LifecycleFn, Module, ModuleDef,
    ProviderDef, Qualified, Resolver, Scope, bind, hooks,
};

// --- missing dependencies ------------------------------------------------------------------

service!(Database);
service!(Mailer);
service!(UserService: Database);

module!(DatabaseModule, |m| {
    m.provide::<Database>().export::<Database>();
});
module!(PrivateDatabaseModule, |m| {
    m.provide::<Database>();
});

#[test]
fn missing_dependency_suggests_importing_the_exporting_module() {
    module!(UsersModule, |m| {
        m.provide::<UserService>();
    });
    module!(App, |m| {
        m.import(DatabaseModule).import(UsersModule);
    });
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn missing_dependency_suggests_exporting_a_private_provider() {
    module!(UsersModule, |m| {
        m.import(PrivateDatabaseModule).provide::<UserService>();
    });
    let err = Hornero::new(UsersModule).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn missing_dependency_without_any_provider() {
    module!(App, |m| {
        m.provide::<UserService>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn all_problems_are_reported_together() {
    service!(Notifier: Mailer);
    module!(App, |m| {
        m.provide::<UserService>().provide::<Notifier>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 2);
}

// --- visibility ----------------------------------------------------------------------------

#[test]
fn imported_exports_are_visible() {
    module!(UsersModule, |m| {
        m.import(DatabaseModule).provide::<UserService>();
    });
    Hornero::new(UsersModule).validate().unwrap();
}

#[test]
fn exports_are_not_transitive_without_reexport() {
    module!(CoreModule, |m| {
        m.import(DatabaseModule);
    });
    module!(UsersModule, |m| {
        m.import(CoreModule).provide::<UserService>();
    });
    let err = Hornero::new(UsersModule).validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("add it to the imports of UsersModule"),
        "{err}"
    );
}

#[test]
fn reexported_modules_are_visible() {
    module!(CoreModule, |m| {
        m.import(DatabaseModule).export_module::<DatabaseModule>();
    });
    module!(UsersModule, |m| {
        m.import(CoreModule).provide::<UserService>();
    });
    Hornero::new(UsersModule).validate().unwrap();
}

#[test]
fn reexported_providers_are_visible() {
    module!(CoreModule, |m| {
        m.import(DatabaseModule).export::<Database>();
    });
    module!(UsersModule, |m| {
        m.import(CoreModule).provide::<UserService>();
    });
    Hornero::new(UsersModule).validate().unwrap();
}

#[test]
fn global_module_exports_are_visible_everywhere() {
    module!(GlobalDatabaseModule, |m| {
        m.global().provide::<Database>().export::<Database>();
    });
    module!(UsersModule, |m| {
        m.provide::<UserService>();
    });
    module!(App, |m| {
        m.import(GlobalDatabaseModule).import(UsersModule);
    });
    Hornero::new(App).validate().unwrap();
}

#[test]
fn ambiguous_imports_are_rejected() {
    module!(OtherDatabaseModule, |m| {
        m.provide::<Database>().export::<Database>();
    });
    module!(UsersModule, |m| {
        m.import(DatabaseModule)
            .import(OtherDatabaseModule)
            .provide::<UserService>();
    });
    let err = Hornero::new(UsersModule).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn invalid_export_is_rejected() {
    module!(App, |m| {
        m.export::<Database>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn duplicate_provider_in_one_module_is_rejected() {
    module!(App, |m| {
        m.provide::<Database>().provide_value(Database());
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("`Database` is registered more than once in App"),
        "{err}"
    );
}

// --- cycles --------------------------------------------------------------------------------

service!(A: B);
service!(B: C);
service!(C: A);

#[test]
fn circular_dependencies_report_the_path() {
    module!(App, |m| {
        m.provide::<A>().provide::<B>().provide::<C>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn circular_imports_report_the_path() {
    struct Left;
    struct Right;
    impl Module for Left {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Right);
        }
    }
    impl Module for Right {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Left);
        }
    }
    let err = Hornero::new(Left).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn instances_importing_each_other_report_the_path() {
    struct Ping(u32);
    struct Pong(u32);
    impl Module for Ping {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Pong(self.0));
        }

        fn identity(&self) -> Identity {
            Identity::Instance
        }
    }
    impl Module for Pong {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Ping(self.0));
        }

        fn identity(&self) -> Identity {
            Identity::Instance
        }
    }
    let err = Hornero::new(Ping(1)).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn a_cycle_ends_with_the_module_being_imported() {
    struct Named(u32);
    impl Module for Named {
        fn define(&self, m: &mut ModuleDef) {
            if self.0 == 0 {
                m.import(Named(1));
            }
        }

        fn identity(&self) -> Identity {
            Identity::Instance
        }

        fn name(&self) -> String {
            format!("Named{}", self.0)
        }
    }
    let err = Hornero::new(Named(0)).validate().unwrap_err();
    assert!(err.to_string().contains("Named0 -> Named1"), "{err}");
}

/// A stack of modules of one type, told apart by their keys.
struct Layered {
    base: bool,
}

impl Module for Layered {
    fn define(&self, m: &mut ModuleDef) {
        if !self.base {
            m.import(Layered { base: true });
        }
    }

    fn identity(&self) -> Identity {
        Identity::key(self.base)
    }
}

#[test]
fn keyed_modules_of_one_type_can_nest() {
    Hornero::new(Layered { base: false }).validate().unwrap();
}

#[test]
fn a_keyed_module_importing_its_own_key_is_a_cycle() {
    struct Looping(u32);
    impl Module for Looping {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Looping(self.0));
        }

        fn identity(&self) -> Identity {
            Identity::key(self.0)
        }
    }
    let err = Hornero::new(Looping(1)).validate().unwrap_err();
    assert!(err.to_string().contains("Looping -> Looping"), "{err}");
}

// --- construction --------------------------------------------------------------------------

static COUNTED_BUILDS: AtomicUsize = AtomicUsize::new(0);

struct Counted;

impl Injectable for Counted {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }

    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        COUNTED_BUILDS.fetch_add(1, Ordering::SeqCst);
        Ok(Counted)
    }
}

#[tokio::test]
async fn modules_imported_twice_are_instantiated_once() {
    module!(SharedModule, |m| {
        m.provide::<Counted>().export::<Counted>();
    });
    module!(Left, |m| {
        m.import(SharedModule);
    });
    module!(Right, |m| {
        m.import(SharedModule);
    });
    module!(App, |m| {
        m.import(Left).import(Right);
    });
    let before = COUNTED_BUILDS.load(Ordering::SeqCst);
    Hornero::new(App).build().await.unwrap();
    assert_eq!(COUNTED_BUILDS.load(Ordering::SeqCst) - before, 1);
}

// --- module identity -----------------------------------------------------------------------

pub struct Ttl(u64);

/// A dynamic module: every import is a module of its own, like Nest's `forRoot`.
struct CacheModule {
    ttl: u64,
}

impl Module for CacheModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide_value(Ttl(self.ttl)).export::<Ttl>();
    }

    fn identity(&self) -> Identity {
        Identity::Instance
    }
}

service!(Sessions: Ttl);
service!(Tokens: Ttl);

#[tokio::test]
async fn instance_modules_are_one_module_per_import() {
    module!(SessionsModule, |m| {
        m.import(CacheModule { ttl: 1 })
            .provide::<Sessions>()
            .export::<Sessions>();
    });
    module!(TokensModule, |m| {
        m.import(CacheModule { ttl: 60 })
            .provide::<Tokens>()
            .export::<Tokens>();
    });
    module!(App, |m| {
        m.import(SessionsModule).import(TokensModule);
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<Sessions>().unwrap().0.0, 1);
    assert_eq!(app.get::<Tokens>().unwrap().0.0, 60);
}

/// Carries data without declaring an identity.
struct UndeclaredCache {
    ttl: u64,
}

impl Module for UndeclaredCache {
    fn define(&self, m: &mut ModuleDef) {
        m.provide_value(Ttl(self.ttl)).export::<Ttl>();
    }
}

#[test]
fn modules_with_data_imported_twice_must_declare_their_identity() {
    module!(SessionsModule, |m| {
        m.import(UndeclaredCache { ttl: 1 }).provide::<Sessions>();
    });
    module!(TokensModule, |m| {
        m.import(UndeclaredCache { ttl: 60 }).provide::<Tokens>();
    });
    module!(App, |m| {
        m.import(SessionsModule).import(TokensModule);
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn a_module_with_data_imported_once_needs_no_identity() {
    module!(App, |m| {
        m.import(UndeclaredCache { ttl: 1 }).provide::<Sessions>();
    });
    Hornero::new(App).validate().unwrap();
}

service!(Reports: Database);
service!(Billing: Database);
service!(Audit: Database);

#[tokio::test]
async fn zero_sized_modules_are_shared_even_with_phantom_data() {
    struct Tagged<T>(PhantomData<fn() -> T>);
    impl<T: 'static> Module for Tagged<T> {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Database>().export::<Database>();
        }
    }
    module!(ReportsModule, |m| {
        m.import(Tagged::<u8>(PhantomData))
            .provide::<Reports>()
            .export::<Reports>();
    });
    module!(BillingModule, |m| {
        m.import(Tagged::<u8>(PhantomData))
            .provide::<Billing>()
            .export::<Billing>();
    });
    module!(App, |m| {
        m.import(ReportsModule).import(BillingModule);
    });
    let app = Hornero::new(App).build().await.unwrap();
    let reports = app.get::<Reports>().unwrap();
    assert!(Arc::ptr_eq(&reports.0, &app.get::<Billing>().unwrap().0));
}

/// Imports with equal keys are one module.
struct PoolModule {
    name: &'static str,
}

impl Module for PoolModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide::<Database>().export::<Database>();
    }

    fn identity(&self) -> Identity {
        Identity::key(self.name)
    }
}

#[tokio::test]
async fn modules_with_equal_keys_are_one_module() {
    module!(ReportsModule, |m| {
        m.import(PoolModule { name: "main" })
            .provide::<Reports>()
            .export::<Reports>();
    });
    module!(BillingModule, |m| {
        m.import(PoolModule { name: "main" })
            .provide::<Billing>()
            .export::<Billing>();
    });
    module!(AuditModule, |m| {
        m.import(PoolModule { name: "audit" })
            .provide::<Audit>()
            .export::<Audit>();
    });
    module!(App, |m| {
        m.import(ReportsModule)
            .import(BillingModule)
            .import(AuditModule);
    });
    let app = Hornero::new(App).build().await.unwrap();
    let reports = app.get::<Reports>().unwrap();
    assert!(Arc::ptr_eq(&reports.0, &app.get::<Billing>().unwrap().0));
    assert!(!Arc::ptr_eq(&reports.0, &app.get::<Audit>().unwrap().0));
}

#[test]
fn instance_modules_are_re_exported_by_type() {
    module!(StorageModule, |m| {
        m.import(CacheModule { ttl: 5 })
            .export_module::<CacheModule>();
    });
    module!(App, |m| {
        m.import(StorageModule).provide::<Sessions>();
    });
    Hornero::new(App).validate().unwrap();
}

#[test]
fn re_exporting_two_instances_of_one_module_type_is_ambiguous() {
    module!(StorageModule, |m| {
        m.import(CacheModule { ttl: 1 })
            .import(CacheModule { ttl: 60 })
            .export_module::<CacheModule>();
    });
    module!(App, |m| {
        m.import(StorageModule);
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn an_ambiguous_key_exported_both_ways_is_reported_once() {
    module!(StorageModule, |m| {
        m.import(CacheModule { ttl: 1 })
            .import(CacheModule { ttl: 60 })
            .export::<Ttl>()
            .export_module::<CacheModule>();
    });
    module!(App, |m| {
        m.import(StorageModule);
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
}

#[tokio::test]
async fn explicit_exports_win_over_module_re_exports() {
    struct Name(&'static str);
    service!(Greeter: Name);
    module!(Library, |m| {
        m.provide_value(Name("imported")).export::<Name>();
    });
    module!(Mid, |m| {
        m.import(Library)
            .provide_value(Name("local"))
            .export::<Name>()
            .export_module::<Library>();
    });
    module!(App, |m| {
        m.import(Mid).provide::<Greeter>();
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<Greeter>().unwrap().0.0, "local");
}

#[test]
fn re_exporting_a_module_that_is_not_imported_says_to_import_it() {
    module!(App, |m| {
        m.export_module::<CacheModule>();
    });
    let err = Hornero::new(App).validate().unwrap_err().to_string();
    assert!(
        err.contains("App re-exports the module `CacheModule`, but does not import it"),
        "{err}"
    );
    assert!(err.contains("hint: import `CacheModule` in App."), "{err}");
}

#[tokio::test]
async fn local_providers_shadow_imported_ones() {
    struct Name(&'static str);
    service!(Greeter: Name);
    module!(Library, |m| {
        m.provide_value(Name("imported")).export::<Name>();
    });
    module!(App, |m| {
        m.import(Library)
            .provide_value(Name("local"))
            .provide::<Greeter>();
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<Greeter>().unwrap().0.0, "local");
}

#[tokio::test]
async fn transient_providers_are_created_per_consumer() {
    service!(Fresh; scope = Scope::Transient);
    service!(First: Fresh);
    service!(Second: Fresh);
    module!(App, |m| {
        m.provide::<Fresh>().provide::<First>().provide::<Second>();
    });
    let app = Hornero::new(App).build().await.unwrap();
    let first = app.get::<First>().unwrap();
    let second = app.get::<Second>().unwrap();
    assert!(!Arc::ptr_eq(&first.0, &second.0));
    assert!(
        app.get::<Fresh>().is_none(),
        "transient providers are not cached"
    );
}

trait Repo: Send + Sync {
    fn backend(&self) -> &'static str;
}

service!(PgRepo);

impl Repo for PgRepo {
    fn backend(&self) -> &'static str {
        "postgres"
    }
}

struct MemoryRepo;

impl Repo for MemoryRepo {
    fn backend(&self) -> &'static str {
        "memory"
    }
}

struct RepoUser(Arc<dyn Repo>);

impl Injectable for RepoUser {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<dyn Repo>()]
    }

    async fn construct(resolver: &Resolver) -> Result<Self, BoxError> {
        Ok(RepoUser(resolver.get::<dyn Repo>()?))
    }
}

#[tokio::test]
async fn trait_objects_are_bound_to_implementations() {
    module!(App, |m| {
        m.provider(bind!(dyn Repo => PgRepo)).provide::<RepoUser>();
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<RepoUser>().unwrap().0.backend(), "postgres");
    assert!(
        app.get::<PgRepo>().is_none(),
        "bind! does not register the implementation itself"
    );
}

#[tokio::test]
async fn overrides_replace_providers_and_drop_their_dependencies() {
    service!(NeedsMissing: Mailer);
    struct Holder(Arc<dyn Repo>, #[allow(dead_code)] Arc<NeedsMissing>);
    impl Injectable for Holder {
        fn dependencies() -> Vec<Key> {
            vec![Key::of::<dyn Repo>(), Key::of::<NeedsMissing>()]
        }
        async fn construct(r: &Resolver) -> Result<Self, BoxError> {
            Ok(Holder(r.get::<dyn Repo>()?, r.get::<NeedsMissing>()?))
        }
    }
    module!(App, |m| {
        m.provider(bind!(dyn Repo => PgRepo))
            .provide::<NeedsMissing>()
            .provide::<Holder>();
    });

    assert!(Hornero::new(App).validate().is_err(), "Mailer is missing");

    let repo: Arc<dyn Repo> = Arc::new(MemoryRepo);
    let app = Hornero::new(App)
        .override_provider(repo)
        .override_value(NeedsMissing(Arc::new(Mailer())))
        .build()
        .await
        .unwrap();
    assert_eq!(app.get::<Holder>().unwrap().0.backend(), "memory");
}

#[test]
fn unused_overrides_are_rejected() {
    module!(App, |m| {
        m.provide::<Database>();
    });
    let err = Hornero::new(App)
        .override_value(Mailer())
        .validate()
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("override was registered for `Mailer`"),
        "{err}"
    );
}

#[tokio::test]
async fn factories_receive_injected_arguments() {
    struct Config {
        url: String,
    }
    struct Client {
        url: String,
    }
    module!(App, |m| {
        m.provide_value(Config {
            url: "postgres://localhost".into(),
        })
        .factory(|config: Arc<Config>| async move {
            Ok::<_, BoxError>(Client {
                url: format!("{}/app", config.url),
            })
        });
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<Client>().unwrap().url, "postgres://localhost/app");
}

#[tokio::test]
async fn construction_failures_abort_the_bootstrap() {
    struct Flaky;
    module!(App, |m| {
        m.provider(ProviderDef::factory(|| async {
            Err::<Flaky, _>("connection refused")
        }));
    });
    let err = Hornero::new(App).build().await.err().unwrap();
    insta::assert_snapshot!(err.to_string());
}

#[tokio::test]
async fn undeclared_dependencies_are_an_error() {
    struct Sneaky;
    impl Injectable for Sneaky {
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(r: &Resolver) -> Result<Self, BoxError> {
            r.get::<Database>()?;
            Ok(Sneaky)
        }
    }
    module!(App, |m| {
        m.provide::<Database>().provide::<Sneaky>();
    });
    let err = Hornero::new(App).build().await.err().unwrap();
    assert!(
        err.to_string()
            .contains("requested `Database`, which is not one of its declared dependencies"),
        "{err}"
    );
}

// --- qualifiers ----------------------------------------------------------------------------

struct Url(&'static str);
struct Primary;
struct Replica;

struct Replicated {
    primary: Arc<Url>,
    replica: Arc<Url>,
}

impl Injectable for Replicated {
    fn dependencies() -> Vec<Key> {
        vec![
            Key::of::<Url>().qualified::<Primary>(),
            Key::of::<Url>().qualified::<Replica>(),
        ]
    }

    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(Replicated {
            primary: r.resolve::<Qualified<Url, Primary>>()?.into_inner(),
            replica: r.resolve::<Qualified<Url, Replica>>()?.into_inner(),
        })
    }
}

module!(UrlModule, |m| {
    m.provider(ProviderDef::value(Url("primary")).qualified::<Primary>())
        .provider(ProviderDef::value(Url("replica")).qualified::<Replica>())
        .export_qualified::<Url, Primary>()
        .export_qualified::<Url, Replica>();
});

#[tokio::test]
async fn qualified_providers_of_one_type_coexist() {
    struct Pair(String);
    module!(App, |m| {
        m.import(UrlModule).provide::<Replicated>().factory(
            |primary: Qualified<Url, Primary>, replica: Qualified<Url, Replica>| async move {
                Ok::<_, BoxError>(Pair(format!("{}+{}", primary.0, replica.0)))
            },
        );
    });
    let app = Hornero::new(App).build().await.unwrap();
    let replicated = app.get::<Replicated>().unwrap();
    assert_eq!(
        (replicated.primary.0, replicated.replica.0),
        ("primary", "replica")
    );
    assert_eq!(app.get::<Pair>().unwrap().0, "primary+replica");
    assert_eq!(app.get_qualified::<Url, Replica>().unwrap().0, "replica");
    assert!(
        app.get::<Url>().is_none(),
        "no provider of `Url` is unqualified"
    );
}

#[test]
fn duplicate_qualified_providers_are_rejected() {
    module!(App, |m| {
        m.provider(ProviderDef::value(Url("a")).qualified::<Primary>())
            .provider(ProviderDef::value(Url("b")).qualified::<Primary>())
            .provide_value(Url("unqualified"));
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    assert!(
        err.to_string()
            .contains("`Url@Primary` is registered more than once in App"),
        "{err}"
    );
}

#[test]
fn missing_qualified_dependency_lists_the_provided_qualifiers() {
    struct Archive;
    struct Client;
    module!(App, |m| {
        m.import(UrlModule)
            .factory(|_url: Qualified<Url, Archive>| async { Ok::<_, BoxError>(Client) });
    });
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn qualified_exports_must_name_a_provider() {
    module!(App, |m| {
        m.provide_value(Url("unqualified"))
            .export_qualified::<Url, Primary>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("App exports `Url@Primary`, but neither provides nor imports it"),
        "{err}"
    );
}

#[tokio::test]
async fn qualified_providers_can_be_overridden() {
    let app = Hornero::new(UrlModule)
        .override_with(ProviderDef::value(Url("fake")).qualified::<Primary>())
        .build()
        .await
        .unwrap();
    assert_eq!(app.get_qualified::<Url, Primary>().unwrap().0, "fake");
    assert_eq!(app.get_qualified::<Url, Replica>().unwrap().0, "replica");
}

#[test]
fn hooks_apply_to_qualified_providers() {
    struct Pool;
    impl Lifecycle for Pool {}
    let def = ProviderDef::value(Pool)
        .qualified::<Primary>()
        .with_hooks::<Pool>();
    assert_eq!(def.key(), Key::of::<Pool>().qualified::<Primary>());
    // The qualifier does not make `with_hooks::<Pool>()` a type mismatch.
    module!(App, |m| {
        m.provider(
            ProviderDef::value(Pool)
                .qualified::<Primary>()
                .with_hooks::<Pool>(),
        );
    });
    Hornero::new(App).validate().unwrap();
}

// --- optional dependencies -----------------------------------------------------------------

struct Metrics(&'static str);

struct Instrumented(Option<Arc<Metrics>>);

impl Injectable for Instrumented {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<Metrics>().optional()]
    }

    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(Instrumented(r.resolve()?))
    }
}

#[tokio::test]
async fn optional_dependencies_are_none_when_nothing_provides_them() {
    struct Label(Option<String>);
    module!(App, |m| {
        m.provide::<Instrumented>().factory(
            |metrics: Option<Arc<Metrics>>, url: Option<Qualified<Url, Primary>>| async move {
                Ok::<_, BoxError>(Label(metrics.map(|m| format!("{}{}", m.0, url.is_some()))))
            },
        );
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert!(app.get::<Instrumented>().unwrap().0.is_none());
    assert!(app.get::<Label>().unwrap().0.is_none());
}

#[tokio::test]
async fn optional_dependencies_are_injected_when_visible() {
    struct Label(String);
    module!(App, |m| {
        m.import(UrlModule)
            .provide_value(Metrics("prometheus"))
            .provide::<Instrumented>()
            .factory(
                |metrics: Option<Arc<Metrics>>, url: Option<Qualified<Url, Primary>>| async move {
                    Ok::<_, BoxError>(Label(format!("{}:{}", metrics.unwrap().0, url.unwrap().0)))
                },
            );
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(
        app.get::<Instrumented>().unwrap().0.as_ref().unwrap().0,
        "prometheus"
    );
    assert_eq!(app.get::<Label>().unwrap().0, "prometheus:primary");
}

#[test]
fn optional_dependencies_still_reject_ambiguity() {
    module!(LeftMetrics, |m| {
        m.provide_value(Metrics("left")).export::<Metrics>();
    });
    module!(RightMetrics, |m| {
        m.provide_value(Metrics("right")).export::<Metrics>();
    });
    module!(App, |m| {
        m.import(LeftMetrics)
            .import(RightMetrics)
            .provide::<Instrumented>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("requires `Metrics`, which is visible from several sources"),
        "{err}"
    );
}

#[tokio::test]
async fn absent_optional_dependencies_cannot_be_required() {
    struct Strict;
    impl Injectable for Strict {
        fn dependencies() -> Vec<Key> {
            vec![Key::of::<Metrics>().optional()]
        }
        async fn construct(r: &Resolver) -> Result<Self, BoxError> {
            r.get::<Metrics>()?;
            Ok(Strict)
        }
    }
    module!(App, |m| {
        m.provide::<Strict>();
    });
    let err = Hornero::new(App).build().await.err().unwrap();
    assert!(
        err.to_string()
            .contains("requested `Metrics`, an optional dependency that no module provides"),
        "{err}"
    );
}

// --- scopes --------------------------------------------------------------------------------

service!(AuditLog; scope = Scope::Request);
service!(TodoService: Database, AuditLog);
service!(TodoFacade: TodoService);

#[test]
fn request_scope_bubbles_up_and_is_reported() {
    module!(App, |m| {
        m.provide::<Database>()
            .provide::<AuditLog>()
            .provide::<TodoService>()
            .provide::<TodoFacade>();
    });
    let report = Hornero::new(App).validate().unwrap();
    insta::assert_snapshot!(report.to_string());
    let facade = report.find("TodoFacade").unwrap();
    assert!(!facade.declared);
    assert_eq!(
        facade.chain,
        ["TodoFacade", "TodoService", "AuditLog [request]"]
    );
    assert!(report.find("AuditLog").unwrap().declared);
    assert!(report.find("Database").is_none());
}

#[test]
fn pinned_providers_reject_bubbling() {
    service!(PinnedService: AuditLog; scope = Scope::Singleton, pinned = true);
    module!(App, |m| {
        m.provide::<AuditLog>().provide::<PinnedService>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

struct Hooked(#[allow(dead_code)] Arc<AuditLog>);

impl Injectable for Hooked {
    const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();

    fn dependencies() -> Vec<Key> {
        vec![Key::of::<AuditLog>()]
    }

    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(Hooked(r.get::<AuditLog>()?))
    }
}

impl Lifecycle for Hooked {}

#[test]
fn lifecycle_hooks_require_singletons() {
    module!(App, |m| {
        m.provide::<AuditLog>().provide::<Hooked>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn request_scope_through_request_context() {
    struct CurrentPath(#[allow(dead_code)] Arc<hornero_core::RequestContext>);
    impl Injectable for CurrentPath {
        fn dependencies() -> Vec<Key> {
            vec![Key::of::<hornero_core::RequestContext>()]
        }
        async fn construct(r: &Resolver) -> Result<Self, BoxError> {
            Ok(CurrentPath(r.get()?))
        }
    }
    module!(App, |m| {
        m.provide::<CurrentPath>();
    });
    let report = Hornero::new(App).validate().unwrap();
    assert_eq!(
        report.find("CurrentPath").unwrap().chain,
        ["CurrentPath", "RequestContext [request]"]
    );
}

// --- override precedence, re-exports and registration mistakes ------------------------------------

#[tokio::test]
async fn the_last_override_for_a_key_wins() {
    module!(App, |m| {
        m.provider(bind!(dyn Repo => PgRepo));
    });
    let first: Arc<dyn Repo> = Arc::new(PgRepo());
    let second: Arc<dyn Repo> = Arc::new(MemoryRepo);
    let app = Hornero::new(App)
        .override_provider(first)
        .override_provider(second)
        .build()
        .await
        .unwrap();
    assert_eq!(app.get::<dyn Repo>().unwrap().backend(), "memory");
}

#[test]
fn a_replaced_override_is_not_reported_as_unused() {
    module!(App, |m| {
        m.provide::<Database>();
    });
    Hornero::new(App)
        .override_value(Database())
        .override_value(Database())
        .validate()
        .unwrap();
}

#[test]
fn ambiguous_re_exports_are_rejected() {
    module!(OtherDatabaseModule, |m| {
        m.provide::<Database>().export::<Database>();
    });
    module!(FacadeModule, |m| {
        m.import(DatabaseModule)
            .import(OtherDatabaseModule)
            .export::<Database>();
    });
    module!(App, |m| {
        m.import(FacadeModule);
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn re_exporting_one_imported_provider_is_fine() {
    module!(FacadeModule, |m| {
        m.import(DatabaseModule).export::<Database>();
    });
    module!(App, |m| {
        m.import(FacadeModule).provide::<UserService>();
    });
    Hornero::new(App).validate().unwrap();
}

#[test]
fn with_hooks_on_the_wrong_type_is_a_bootstrap_error() {
    module!(App, |m| {
        m.provider(ProviderDef::value(Mailer()).with_hooks::<Hooked>());
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    insta::assert_snapshot!(err.to_string());
}

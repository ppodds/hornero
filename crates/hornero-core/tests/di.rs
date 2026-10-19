mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hornero_core::{
    BoxError, Hornero, Injectable, Key, Lifecycle, LifecycleFn, Module, ModuleDef, ModuleId,
    Problem, ProviderDef, Qualified, Resolver, Scope, Shutdown, bind, hooks,
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

// --- re-exported modules -------------------------------------------------------------------

pub struct Region(&'static str);
service!(Weather: Region);

#[tokio::test]
async fn an_exported_local_provider_shadows_a_re_exported_module() {
    module!(Library, |m| {
        m.provide_value(Region("imported")).export::<Region>();
    });
    module!(Facade, |m| {
        m.import(Library)
            .provide_value(Region("local"))
            .export::<Region>()
            .export_module::<Library>();
    });
    module!(App, |m| {
        m.import(Facade).provide::<Weather>();
    });
    let app = Hornero::new(App).build().await.unwrap();
    // Importers see the provider Facade itself uses, as with any other local provider.
    assert_eq!(app.get::<Weather>().unwrap().0.0, "local");
}

#[test]
fn re_exported_modules_that_disagree_are_rejected() {
    module!(North, |m| {
        m.provide_value(Region("north")).export::<Region>();
    });
    module!(South, |m| {
        m.provide_value(Region("south")).export::<Region>();
    });
    module!(Facade, |m| {
        m.import(North)
            .import(South)
            .export_module::<North>()
            .export_module::<South>();
    });
    module!(App, |m| {
        m.import(Facade).provide::<Weather>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn one_provider_re_exported_along_several_paths_is_not_ambiguous() {
    module!(Library, |m| {
        m.provide_value(Region("library")).export::<Region>();
    });
    module!(Wrapper, |m| {
        m.import(Library).export_module::<Library>();
    });
    module!(Facade, |m| {
        m.import(Library)
            .import(Wrapper)
            .export_module::<Library>()
            .export_module::<Wrapper>();
    });
    module!(App, |m| {
        m.import(Facade).provide::<Weather>();
    });
    Hornero::new(App).validate().unwrap();
}

#[test]
fn re_exporting_a_module_that_is_not_imported_is_rejected() {
    module!(Library, |m| {
        m.provide_value(Region("library")).export::<Region>();
    });
    module!(App, |m| {
        m.export_module::<Library>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        matches!(err.problems(), [Problem::InvalidExport { module, .. }] if module == "App"),
        "{err}"
    );
}

// --- global and dynamic modules ------------------------------------------------------------

#[test]
fn global_modules_must_still_be_registered_once() {
    module!(GlobalDatabaseModule, |m| {
        m.global().provide::<Database>().export::<Database>();
    });
    module!(UsersModule, |m| {
        m.provide::<UserService>();
    });
    module!(Unregistered, |m| {
        m.import(UsersModule);
    });
    module!(Registered, |m| {
        m.import(GlobalDatabaseModule).import(UsersModule);
    });
    let err = Hornero::new(Unregistered).validate().unwrap_err();
    assert!(
        matches!(err.problems(), [Problem::MissingDependency { .. }]),
        "{err}"
    );
    Hornero::new(Registered).validate().unwrap();
}

#[test]
fn several_global_modules_exporting_one_type_are_ambiguous() {
    module!(LeftGlobal, |m| {
        m.global().provide::<Database>().export::<Database>();
    });
    module!(RightGlobal, |m| {
        m.global().provide_value(Database()).export::<Database>();
    });
    module!(UsersModule, |m| {
        m.provide::<UserService>();
    });
    module!(App, |m| {
        m.import(LeftGlobal).import(RightGlobal).import(UsersModule);
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string().contains(
            "UserService (in UsersModule) requires `Database`, which is visible from several sources"
        ),
        "{err}"
    );
}

#[tokio::test]
async fn imports_take_precedence_over_global_modules() {
    module!(Everywhere, |m| {
        m.global()
            .provide_value(Region("global"))
            .export::<Region>();
    });
    module!(Local, |m| {
        m.provide_value(Region("imported")).export::<Region>();
    });
    module!(Forecasts, |m| {
        m.import(Local).provide::<Weather>().export::<Weather>();
    });
    module!(App, |m| {
        m.import(Everywhere).import(Forecasts);
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<Weather>().unwrap().0.0, "imported");
}

struct Tenant(&'static str);

/// A dynamic module (Nest's `forRoot`): one type, configured per instance.
#[derive(Clone, Copy)]
struct TenantModule {
    tenant: &'static str,
    discriminator: u64,
}

impl Module for TenantModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide_value(Tenant(self.tenant)).export::<Tenant>();
    }

    fn id(&self) -> ModuleId {
        ModuleId::of::<Self>().with_discriminator(self.discriminator)
    }
}

struct LeftTenant(Arc<Tenant>);
struct RightTenant(Arc<Tenant>);

/// Two sibling modules, each importing its own `TenantModule` and injecting the `Tenant`.
fn tenant_app(left: TenantModule, right: TenantModule) -> Hornero {
    struct Left(TenantModule);
    impl Module for Left {
        fn define(&self, m: &mut ModuleDef) {
            m.import(self.0)
                .factory(|t: Arc<Tenant>| async move { Ok::<_, BoxError>(LeftTenant(t)) });
        }
    }
    struct Right(TenantModule);
    impl Module for Right {
        fn define(&self, m: &mut ModuleDef) {
            m.import(self.0)
                .factory(|t: Arc<Tenant>| async move { Ok::<_, BoxError>(RightTenant(t)) });
        }
    }
    struct App(TenantModule, TenantModule);
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Left(self.0)).import(Right(self.1));
        }
    }
    Hornero::new(App(left, right))
}

#[tokio::test]
async fn dynamic_modules_are_told_apart_by_their_discriminator() {
    let app = tenant_app(
        TenantModule {
            tenant: "acme",
            discriminator: 1,
        },
        TenantModule {
            tenant: "globex",
            discriminator: 2,
        },
    )
    .build()
    .await
    .unwrap();
    assert_eq!(app.get::<LeftTenant>().unwrap().0.0, "acme");
    assert_eq!(app.get::<RightTenant>().unwrap().0.0, "globex");
    assert!(
        app.get::<Tenant>().is_none(),
        "two tenants, neither visible from the root"
    );
}

#[tokio::test]
async fn dynamic_modules_with_one_id_are_registered_once_and_the_first_wins() {
    let app = tenant_app(
        TenantModule {
            tenant: "acme",
            discriminator: 7,
        },
        TenantModule {
            tenant: "globex",
            discriminator: 7,
        },
    )
    .build()
    .await
    .unwrap();
    let (left, right) = (
        app.get::<LeftTenant>().unwrap(),
        app.get::<RightTenant>().unwrap(),
    );
    assert!(Arc::ptr_eq(&left.0, &right.0));
    assert_eq!(left.0.0, "acme");
}

#[test]
fn importing_one_dynamic_module_twice_suggests_registering_it_once() {
    module!(App, |m| {
        m.import(TenantModule {
            tenant: "acme",
            discriminator: 1,
        })
        .import(TenantModule {
            tenant: "acme",
            discriminator: 2,
        })
        .factory(|t: Arc<Tenant>| async move { Ok::<_, BoxError>(LeftTenant(t)) });
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("a dynamic module such as `for_root` is registered more than once"),
        "{err}"
    );
}

// --- graph edge cases ----------------------------------------------------------------------

#[test]
fn a_provider_depending_on_itself_is_a_cycle() {
    service!(Ouroboros: Ouroboros);
    module!(App, |m| {
        m.provide::<Ouroboros>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    match err.problems() {
        [Problem::CircularDependency { path }] => {
            assert_eq!(path, &["Ouroboros (in App)", "Ouroboros (in App)"])
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_dependency_declared_twice_is_one_instance() {
    struct Pair(Arc<Database>, Arc<Database>);
    module!(App, |m| {
        m.provide::<Database>()
            .factory(
                |a: Arc<Database>, b: Arc<Database>| async move { Ok::<_, BoxError>(Pair(a, b)) },
            );
    });
    let app = Hornero::new(App).build().await.unwrap();
    let pair = app.get::<Pair>().unwrap();
    assert!(Arc::ptr_eq(&pair.0, &pair.1));
}

#[tokio::test]
async fn optional_dependencies_follow_visibility() {
    module!(PrivateMetrics, |m| {
        m.provide_value(Metrics("private"));
    });
    module!(App, |m| {
        m.import(PrivateMetrics).provide::<Instrumented>();
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert!(
        app.get::<Instrumented>().unwrap().0.is_none(),
        "a provider another module keeps private is not injected"
    );
}

#[test]
fn transient_providers_cannot_have_hooks() {
    struct Ephemeral;
    impl Injectable for Ephemeral {
        const SCOPE: Scope = Scope::Transient;
        const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Ok(Ephemeral)
        }
    }
    impl Lifecycle for Ephemeral {}
    module!(App, |m| {
        m.provide::<Ephemeral>();
    });
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("Ephemeral (in App) has lifecycle hooks but is transient"),
        "{err}"
    );
}

#[tokio::test]
async fn validate_constructs_nothing() {
    static BUILDS: AtomicUsize = AtomicUsize::new(0);
    struct Unreachable;
    module!(App, |m| {
        m.factory(|| async {
            BUILDS.fetch_add(1, Ordering::SeqCst);
            Err::<Unreachable, _>("database is down")
        });
    });
    // A graph test passes without the infrastructure; `build` is what meets it.
    Hornero::new(App).validate().unwrap();
    assert_eq!(BUILDS.load(Ordering::SeqCst), 0);
    assert!(Hornero::new(App).build().await.is_err());
    assert_eq!(BUILDS.load(Ordering::SeqCst), 1);
}

// --- App::get --------------------------------------------------------------------------------

#[tokio::test]
async fn app_get_only_returns_singletons() {
    module!(App, |m| {
        m.provide::<Database>()
            .provide::<AuditLog>()
            .provide::<TodoService>();
    });
    let app = Hornero::new(App).build().await.unwrap();
    assert!(app.get::<Database>().is_some());
    assert!(app.get::<AuditLog>().is_none(), "declared request-scoped");
    assert!(
        app.get::<TodoService>().is_none(),
        "bubbled into request scope"
    );
    assert!(
        app.get::<Shutdown>().is_some(),
        "built-ins are singletons too"
    );
}

#[tokio::test]
async fn app_get_prefers_the_provider_visible_from_the_root() {
    module!(Hidden, |m| {
        m.provide_value(Region("hidden"));
    });
    module!(AlsoHidden, |m| {
        m.provide_value(Region("also hidden"));
    });
    module!(Exported, |m| {
        m.provide_value(Region("exported")).export::<Region>();
    });
    module!(Visible, |m| {
        m.import(Hidden).import(Exported);
    });
    module!(Invisible, |m| {
        m.import(Hidden).import(AlsoHidden);
    });
    let app = Hornero::new(Visible).build().await.unwrap();
    assert_eq!(app.get::<Region>().unwrap().0, "exported");
    let app = Hornero::new(Invisible).build().await.unwrap();
    assert!(
        app.get::<Region>().is_none(),
        "several private providers and none visible from the root: nothing to prefer"
    );
}

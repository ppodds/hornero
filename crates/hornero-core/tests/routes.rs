mod common;

use std::sync::Arc;

use axum::Router;
use axum::routing::get as axum_get;
use common::get;
use hornero_core::{
    AppGuard, BoxError, Controller, ControllerDef, Guard, GuardContext, Hornero, HttpError,
    Injectable, Key, Metadata, Module, ModuleDef, Resolver, Routes,
};

/// Declares a unit struct with a trivial `Injectable` impl.
macro_rules! unit_injectable {
    ($($name:ident),+) => {$(
        struct $name;
        impl Injectable for $name {
            fn dependencies() -> Vec<Key> { Vec::new() }
            async fn construct(_: &Resolver) -> Result<Self, BoxError> { Ok($name) }
        }
    )+};
}

/// Declares unit guards that let every request through.
macro_rules! pass_guards {
    ($($name:ident),+) => {$(
        unit_injectable!($name);
        impl Guard for $name {
            async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
                Ok(())
            }
        }
    )+};
}

pass_guards!(GlobalGuard, ModuleGuard, ControllerGuard, HandlerGuard);

struct Roles(&'static [&'static str]);
struct Public;

/// Constructing it fails, so a test that lists routes proves nothing was constructed.
struct Unbuildable;

impl Injectable for Unbuildable {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }
    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Err("never constructed".into())
    }
}

struct ItemController(#[allow(dead_code)] Arc<Unbuildable>);

impl Injectable for ItemController {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<Unbuildable>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(ItemController(r.get()?))
    }
}

impl Controller for ItemController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.prefix("/items").guard::<ControllerGuard>().meta(Public);
        c.get("/", "list", |_: Arc<Self>| async { "[]" });
        c.delete("/{id}", "remove", |_: Arc<Self>| async { "" })
            .guard::<HandlerGuard>()
            .meta(Roles(&["admin"]));
    }
}

struct ItemModule;

impl Module for ItemModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide::<Unbuildable>()
            .provide::<AppGuard<ModuleGuard>>()
            .controller::<ItemController>();
    }
}

#[test]
fn the_route_table_lists_controller_routes_without_constructing_anything() {
    let routes = Hornero::new(ItemModule)
        .global_prefix("/api/")
        .routes()
        .unwrap();
    assert_eq!(routes.prefix(), "/api");
    assert_eq!(routes.len(), 2);

    let listed: Vec<(String, &str, &str, &str, &str)> = routes
        .iter()
        .map(|route| {
            (
                route.method().to_string(),
                route.path(),
                route.controller(),
                route.handler(),
                route.module(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (
                "GET".into(),
                "/api/items",
                "ItemController",
                "list",
                "ItemModule"
            ),
            (
                "DELETE".into(),
                "/api/items/{id}",
                "ItemController",
                "remove",
                "ItemModule"
            ),
        ]
    );
}

#[test]
fn the_route_table_merges_controller_and_handler_metadata() {
    let routes = Hornero::new(ItemModule).routes().unwrap();
    let [list, remove] = [0, 1].map(|index| routes.iter().nth(index).unwrap());
    assert!(list.meta::<Public>().is_some());
    assert!(list.meta::<Roles>().is_none());
    assert!(remove.meta::<Public>().is_some());
    assert_eq!(
        remove.meta::<Roles>().map(|Roles(roles)| *roles),
        Some(&["admin"][..])
    );
}

#[test]
fn the_route_table_lists_guards_in_the_order_they_run() {
    let routes = Hornero::new(ItemModule)
        .guard::<GlobalGuard>()
        .routes()
        .unwrap();
    let names = |index: usize| -> Vec<String> {
        let route = routes.iter().nth(index).unwrap();
        route
            .guards()
            .iter()
            .map(|guard| guard.name().to_owned())
            .collect()
    };
    assert_eq!(names(0), ["GlobalGuard", "ModuleGuard", "ControllerGuard"]);
    assert_eq!(
        names(1),
        [
            "GlobalGuard",
            "ModuleGuard",
            "ControllerGuard",
            "HandlerGuard"
        ]
    );

    let handler_guard = routes.iter().nth(1).unwrap().guards().last().unwrap();
    assert!(handler_guard.is::<HandlerGuard>());
    assert!(!handler_guard.is::<ControllerGuard>());
}

#[test]
fn the_route_table_reports_invalid_routes_like_validate() {
    unit_injectable!(Dup);
    impl Controller for Dup {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/same", "a", |_: Arc<Self>| async { "" });
            c.get("/same", "b", |_: Arc<Self>| async { "" });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Dup>();
        }
    }
    let err = Hornero::new(App).routes().err().unwrap();
    assert!(err.to_string().contains("GET /same"), "{err}");
}

// --- enhancers describing themselves -------------------------------------------------------

/// What a documentation crate would store: here, just a scheme name.
struct Scheme(&'static str);

struct DescribedGuard;

impl Injectable for DescribedGuard {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }
    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Ok(DescribedGuard)
    }
    fn describe(meta: &mut Metadata) {
        meta.insert(Scheme("bearer"));
    }
}

impl Guard for DescribedGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

unit_injectable!(PingController);

impl Controller for PingController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.get("/ping", "ping", |_: Arc<Self>| async { "pong" });
    }
}

struct PingModule;

impl Module for PingModule {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<PingController>();
    }
}

#[test]
fn guards_describe_themselves_in_the_route_table() {
    let routes = Hornero::new(PingModule)
        .guard::<DescribedGuard>()
        .guard::<GlobalGuard>()
        .routes()
        .unwrap();
    let guards = routes.iter().next().unwrap().guards();
    assert_eq!(
        guards[0].meta::<Scheme>().map(|Scheme(name)| *name),
        Some("bearer")
    );
    assert!(guards[1].meta::<Scheme>().is_none());
}

// --- injecting the table -------------------------------------------------------------------

struct ListingModule;

impl Module for ListingModule {
    fn define(&self, m: &mut ModuleDef) {
        m.import(PingModule)
            .mount("/", |routes: Arc<Routes>| async move {
                let listing: Vec<String> = routes
                    .iter()
                    .map(|route| format!("{} {}", route.method(), route.path()))
                    .collect();
                let body = listing.join("\n");
                Ok::<_, BoxError>(
                    Router::new().route("/routes", axum_get(move || async move { body })),
                )
            });
    }
}

#[tokio::test]
async fn the_route_table_is_injectable() {
    let app = Hornero::new(ListingModule)
        .global_prefix("/v1")
        .build()
        .await
        .unwrap();
    assert_eq!(app.routes().len(), 1);
    assert_eq!(app.routes().prefix(), "/v1");

    let res = get(&app.router(), "/v1/routes").await;
    assert_eq!(res.body, "GET /v1/ping");
}

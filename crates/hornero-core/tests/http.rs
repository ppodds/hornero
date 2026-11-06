mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Extension, Path, Request};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get as axum_get;
use common::domain::DomainApp;
use common::{get, send};
use hornero_core::{
    AppFilter, AppGuard, AppInterceptor, BoxError, Controller, ControllerDef, ExceptionFilter,
    Extracted, Extractor, Guard, GuardContext, Hornero, HttpError, Identity, Injectable,
    Interceptor, Key, Lifecycle, LifecycleFn, Middleware, Module, ModuleDef, Problem, ProviderDef,
    Resolver, RouteContext, Scope, Shutdown, hooks,
};
use serde::{Deserialize, Serialize};

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

/// A layer that tags every response it wraps with `x-{name}: wrapped`.
macro_rules! tag {
    ($name:literal) => {
        axum::middleware::map_response(|mut response: Response| async move {
            response
                .headers_mut()
                .insert(concat!("x-", $name), HeaderValue::from_static("wrapped"));
            response
        })
    };
}

// --- routing and errors --------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
struct Todo {
    id: u32,
    title: String,
}

struct Store(Mutex<Vec<Todo>>);

struct TodoController(Arc<Store>);

impl Injectable for TodoController {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<Store>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(TodoController(r.get()?))
    }
}

impl TodoController {
    async fn find_one(&self, id: u32) -> Result<Json<Todo>, HttpError> {
        let todos = self.0.0.lock().unwrap();
        todos
            .iter()
            .find(|todo| todo.id == id)
            .cloned()
            .map(Json)
            .ok_or_else(|| HttpError::not_found(format!("todo {id} not found")))
    }
}

impl Controller for TodoController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.prefix("/todos");
        c.get(
            "/{id}",
            "find_one",
            |this: Arc<Self>, Path(id): Path<u32>| async move { this.find_one(id).await },
        );
        c.post(
            "/",
            "create",
            |this: Arc<Self>, Json(todo): Json<Todo>| async move {
                this.0.0.lock().unwrap().push(todo.clone());
                (StatusCode::CREATED, Json(todo))
            },
        );
        c.get("/broken", "broken", |_: Arc<Self>| async move {
            Err::<(), HttpError>(std::io::Error::other("disk on fire").into())
        });
        c.get("/panic", "panic", |_: Arc<Self>| async move {
            if true {
                panic!("boom");
            }
        });
    }
}

struct TodoModule;

impl Module for TodoModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide_value(Store(Mutex::new(vec![Todo {
            id: 1,
            title: "write tests".into(),
        }])))
        .controller::<TodoController>();
    }
}

#[tokio::test]
async fn routes_dispatch_to_controller_methods() {
    let app = Hornero::new(TodoModule).build().await.unwrap();
    let router = app.router();

    let res = get(&router, "/todos/1").await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["title"], "write tests");

    let res = send(
        &router,
        Request::post("/todos")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"id":2,"title":"ship it"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED);
    assert_eq!(get(&router, "/todos/2").await.json()["title"], "ship it");
}

#[tokio::test]
async fn http_errors_render_as_problem_json() {
    let router = Hornero::new(TodoModule).build().await.unwrap().router();
    let res = get(&router, "/todos/9").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.headers["content-type"], "application/problem+json");
    assert_eq!(
        res.json(),
        serde_json::json!({
            "type": "about:blank",
            "title": "Not Found",
            "status": 404,
            "detail": "todo 9 not found",
        })
    );
}

#[tokio::test]
async fn std_errors_become_opaque_500s() {
    let router = Hornero::new(TodoModule).build().await.unwrap().router();
    let res = get(&router, "/todos/broken").await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.json()["detail"], "internal server error");
    assert!(!res.body.contains("disk on fire"));
}

#[tokio::test]
async fn panics_become_500s() {
    let router = Hornero::new(TodoModule).build().await.unwrap().router();
    let res = get(&router, "/todos/panic").await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.json()["status"], 500);
}

#[tokio::test]
async fn unknown_routes_get_a_problem_json_404() {
    let router = Hornero::new(TodoModule).build().await.unwrap().router();
    let res = get(&router, "/nope").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.json()["detail"], "no route matches the request");
}

// --- 405 -------------------------------------------------------------------------------------

unit_injectable!(DenyGuard, GuardedFirst, GuardedLast);

impl Guard for DenyGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Err(HttpError::forbidden("deny guard ran"))
    }
}

impl Controller for GuardedFirst {
    fn routes(c: &mut ControllerDef<Self>) {
        c.prefix("/first");
        c.post("/", "post", |_: Arc<Self>| async { "posted" })
            .guard::<DenyGuard>();
        c.get("/", "get", |_: Arc<Self>| async { "got" });
    }
}

impl Controller for GuardedLast {
    fn routes(c: &mut ControllerDef<Self>) {
        c.prefix("/last");
        c.get("/", "get", |_: Arc<Self>| async { "got" });
        c.post("/", "post", |_: Arc<Self>| async { "posted" })
            .guard::<DenyGuard>();
    }
}

struct MethodsModule;

impl Module for MethodsModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide::<DenyGuard>()
            .controller::<GuardedFirst>()
            .controller::<GuardedLast>();
    }
}

#[tokio::test]
async fn a_405_does_not_run_another_routes_guard() {
    let router = Hornero::new(MethodsModule).build().await.unwrap().router();
    for path in ["/first", "/last"] {
        assert_eq!(get(&router, path).await.body, "got", "{path}");
        let put = send(&router, Request::put(path).body(Body::empty()).unwrap()).await;
        assert_eq!(
            put.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{path}: {}",
            put.body
        );
        // The guarded route still guards its own method.
        let post = send(&router, Request::post(path).body(Body::empty()).unwrap()).await;
        assert_eq!(post.status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn a_405_is_problem_json_with_an_allow_header() {
    let router = Hornero::new(MethodsModule).build().await.unwrap().router();
    for path in ["/first", "/last"] {
        let res = send(&router, Request::put(path).body(Body::empty()).unwrap()).await;
        assert_eq!(res.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(res.headers["content-type"], "application/problem+json");
        let allow = res.headers["allow"].to_str().unwrap();
        assert!(allow.contains("GET") && allow.contains("POST"), "{allow}");
        assert_eq!(res.json()["status"], 405);
    }
}

#[tokio::test]
async fn global_prefix_applies_to_routes() {
    let router = Hornero::new(TodoModule)
        .global_prefix("/api/")
        .build()
        .await
        .unwrap()
        .router();
    assert_eq!(get(&router, "/api/todos/1").await.status, StatusCode::OK);
    assert_eq!(get(&router, "/todos/1").await.status, StatusCode::NOT_FOUND);
}

#[test]
fn duplicate_routes_are_rejected() {
    unit_injectable!(Other);
    impl Controller for Other {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/todos/{todo_id}", "shadow", |_: Arc<Self>| async {
                "shadow"
            });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(TodoModule).controller::<Other>();
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn colon_path_parameters_are_rejected() {
    unit_injectable!(Legacy);
    impl Controller for Legacy {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/items/:id", "find", |_: Arc<Self>| async { "" });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Legacy>();
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string().contains("axum 0.8 expects `{param}`"),
        "{err}"
    );
}

#[test]
fn unsupported_methods_are_rejected() {
    unit_injectable!(Cache);
    impl Controller for Cache {
        fn routes(c: &mut ControllerDef<Self>) {
            let purge = axum::http::Method::from_bytes(b"PURGE").unwrap();
            c.route(purge, "/cache", "purge", |_: Arc<Self>| async { "" });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Cache>();
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string().contains("uses the HTTP method `PURGE`"),
        "{err}"
    );
}

// --- guards and metadata -------------------------------------------------------------------

#[derive(Clone, Debug)]
struct User {
    name: String,
    role: &'static str,
}

struct Roles(&'static [&'static str]);

unit_injectable!(AuthGuard, RolesGuard, AllowAll);

impl Guard for AuthGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let header = ctx
            .parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| HttpError::unauthorized("missing bearer token"))?;
        let (name, role) = header.split_once(':').unwrap_or((header, "user"));
        let role = if role == "admin" { "admin" } else { "user" };
        ctx.parts.extensions.insert(User {
            name: name.to_string(),
            role,
        });
        Ok(())
    }
}

impl Guard for RolesGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let Some(Roles(roles)) = ctx.route.meta::<Roles>() else {
            return Ok(());
        };
        let user = ctx.parts.extensions.get::<User>();
        match user {
            Some(user) if roles.contains(&user.role) => Ok(()),
            _ => Err(HttpError::forbidden("insufficient role")),
        }
    }
}

impl Guard for AllowAll {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        ctx.parts.extensions.insert(User {
            name: "test".into(),
            role: "admin",
        });
        Ok(())
    }
}

unit_injectable!(AdminController);

impl Controller for AdminController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.prefix("/admin")
            .guard::<AuthGuard>()
            .guard::<RolesGuard>();
        c.get(
            "/me",
            "me",
            |_: Arc<Self>, Extension(user): Extension<User>| async move { user.name },
        );
        c.get("/stats", "stats", |_: Arc<Self>| async { "42" })
            .meta(Roles(&["admin"]));
    }
}

struct AdminModule;

impl Module for AdminModule {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<AdminController>();
    }
}

fn bearer(uri: &str, token: &str) -> Request<Body> {
    Request::get(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn guards_reject_and_enrich_requests() {
    let router = Hornero::new(AdminModule).build().await.unwrap().router();

    let res = get(&router, "/admin/me").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.headers["www-authenticate"], "Bearer");

    let res = send(&router, bearer("/admin/me", "alice")).await;
    assert_eq!((res.status, res.body.as_str()), (StatusCode::OK, "alice"));

    let res = send(&router, bearer("/admin/stats", "alice")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(res.json()["detail"], "insufficient role");

    let res = send(&router, bearer("/admin/stats", "root:admin")).await;
    assert_eq!((res.status, res.body.as_str()), (StatusCode::OK, "42"));
}

#[tokio::test]
async fn guards_can_be_overridden_with_any_guard() {
    let router = Hornero::new(AdminModule)
        .override_guard::<AuthGuard>(AllowAll)
        .build()
        .await
        .unwrap()
        .router();
    let res = get(&router, "/admin/stats").await;
    assert_eq!((res.status, res.body.as_str()), (StatusCode::OK, "42"));
}

// --- interceptor ordering ------------------------------------------------------------------

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

macro_rules! recording_interceptor {
    ($($name:ident),+) => {$(
        struct $name(Arc<Recorder>);
        impl Injectable for $name {
            fn dependencies() -> Vec<Key> { vec![Key::of::<Recorder>()] }
            async fn construct(r: &Resolver) -> Result<Self, BoxError> { Ok($name(r.get()?)) }
        }
        impl Interceptor for $name {
            async fn intercept(&self, ctx: &RouteContext, req: Request, next: Next) -> Response {
                self.0.0.lock().unwrap().push(format!("{} > {}", stringify!($name), ctx.handler()));
                let response = next.run(req).await;
                self.0.0.lock().unwrap().push(format!("{} <", stringify!($name)));
                response
            }
        }
    )+};
}

recording_interceptor!(GlobalTrace, ControllerTrace, HandlerTrace);

unit_injectable!(TracedController);

impl Controller for TracedController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.interceptor::<ControllerTrace>();
        c.get("/traced", "traced", |_: Arc<Self>| async { "ok" })
            .interceptor::<HandlerTrace>();
    }
}

#[tokio::test]
async fn interceptors_nest_global_controller_handler() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide_value(Recorder::default())
                .export::<Recorder>()
                .controller::<TracedController>();
        }
    }
    let app = Hornero::new(App)
        .interceptor::<GlobalTrace>()
        .build()
        .await
        .unwrap();
    assert_eq!(get(&app.router(), "/traced").await.status, StatusCode::OK);
    let recorded = app.get::<Recorder>().unwrap().0.lock().unwrap().clone();
    assert_eq!(
        recorded,
        [
            "GlobalTrace > traced",
            "ControllerTrace > traced",
            "HandlerTrace > traced",
            "HandlerTrace <",
            "ControllerTrace <",
            "GlobalTrace <",
        ]
    );
}

// --- request data --------------------------------------------------------------------------

unit_injectable!(ProfileController, BusyController);

impl Controller for ProfileController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.guard::<AuthGuard>();
        c.get(
            "/profile",
            "profile",
            |_: Arc<Self>, Extension(user): Extension<User>| async move {
                format!("{} ({})", user.name, user.role)
            },
        );
    }
}

impl Controller for BusyController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.guard::<AuthGuard>();
        c.get("/busy", "busy", |_: Arc<Self>| async {
            Err::<String, _>(HttpError::conflict("busy"))
        });
    }
}

struct BusyModule;

impl Module for BusyModule {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<BusyController>();
    }
}

#[tokio::test]
async fn extractors_see_data_written_by_guards() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<ProfileController>();
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();
    let res = send(&router, bearer("/profile", "bob:admin")).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (StatusCode::OK, "bob (admin)")
    );
}

// Names the authenticated user in error responses. Interceptors run after the guards and own the
// request, so what a filter cannot see is available here.
unit_injectable!(UserErrors);

impl Interceptor for UserErrors {
    async fn intercept(&self, _: &RouteContext, req: Request, next: Next) -> Response {
        let user = req.extensions().get::<User>().cloned();
        let mut response = next.run(req).await;
        if let Some(err) = HttpError::in_response(&response) {
            let name = user.map_or("anonymous".into(), |user| user.name);
            let body = format!("{} for {name}", err.message());
            *response.body_mut() = Body::from(body);
        }
        response
    }
}

#[tokio::test]
async fn interceptors_render_errors_from_what_guards_wrote() {
    let router = Hornero::new(BusyModule)
        .interceptor::<UserErrors>()
        .build()
        .await
        .unwrap()
        .router();
    let res = send(&router, bearer("/busy", "bob")).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (StatusCode::CONFLICT, "busy for bob")
    );
}

#[tokio::test]
async fn filters_run_after_interceptors_that_keep_the_error() {
    let router = Hornero::new(BusyModule)
        .interceptor::<UserErrors>()
        .filter::<GlobalFilter>()
        .build()
        .await
        .unwrap()
        .router();
    let res = send(&router, bearer("/busy", "bob")).await;
    assert_eq!(res.body, "global: busy at /busy");
}

// Names the path in error responses. Middleware wraps the guards too, so it sees their rejections.
unit_injectable!(PathErrors);

impl Middleware for PathErrors {
    async fn handle(&self, req: Request, next: Next) -> Response {
        let path = req.uri().path().to_owned();
        let mut response = next.run(req).await;
        if let Some(err) = HttpError::in_response(&response) {
            let body = format!("{} at {path}", err.message());
            *response.body_mut() = Body::from(body);
        }
        response
    }
}

#[tokio::test]
async fn middleware_renders_guard_rejections_from_the_request() {
    let router = Hornero::new(BusyModule)
        .middleware::<PathErrors>()
        .build()
        .await
        .unwrap()
        .router();
    let res = get(&router, "/busy").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(res.body.ends_with(" at /busy"), "{}", res.body);
}

// --- exception filters ---------------------------------------------------------------------

macro_rules! labelled_filter {
    ($($name:ident => $label:literal),+) => {$(
        unit_injectable!($name);
        impl ExceptionFilter for $name {
            async fn catch(&self, err: &HttpError, ctx: &RouteContext) -> Response {
                (err.status(), format!("{}: {} at {}", $label, err.message(), ctx.path())).into_response()
            }
        }
    )+};
}

labelled_filter!(GlobalFilter => "global", ControllerFilter => "controller", HandlerFilter => "handler");

unit_injectable!(FilteredController, PlainController);

impl Controller for FilteredController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.prefix("/filtered").filter::<ControllerFilter>();
        c.get("/a", "a", |_: Arc<Self>| async {
            HttpError::conflict("a failed")
        });
        c.get("/b", "b", |_: Arc<Self>| async {
            HttpError::conflict("b failed")
        })
        .filter::<HandlerFilter>();
        c.get("/ok", "ok", |_: Arc<Self>| async { "fine" });
    }
}

impl Controller for PlainController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.get("/plain", "plain", |_: Arc<Self>| async {
            HttpError::bad_request("plain failed")
        });
    }
}

#[tokio::test]
async fn the_most_specific_filter_handles_errors() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<FilteredController>()
                .controller::<PlainController>();
        }
    }
    let router = Hornero::new(App)
        .filter::<GlobalFilter>()
        .build()
        .await
        .unwrap()
        .router();

    let res = get(&router, "/filtered/a").await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    assert_eq!(res.body, "controller: a failed at /filtered/a");
    assert_eq!(
        get(&router, "/filtered/b").await.body,
        "handler: b failed at /filtered/b"
    );
    assert_eq!(
        get(&router, "/plain").await.body,
        "global: plain failed at /plain"
    );
    assert_eq!(get(&router, "/filtered/ok").await.body, "fine");
}

#[tokio::test]
async fn filters_also_see_guard_rejections() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<AdminController>();
        }
    }
    let router = Hornero::new(App)
        .filter::<GlobalFilter>()
        .build()
        .await
        .unwrap()
        .router();
    assert_eq!(
        get(&router, "/admin/me").await.body,
        "global: missing bearer token at /admin/me"
    );
}

// --- domain errors raised with `?` ----------------------------------------------------------

#[tokio::test]
async fn filters_catch_domain_errors_raised_with_the_question_mark() {
    let router = Hornero::new(DomainApp)
        .filter::<common::domain::TodoErrorFilter>()
        .build()
        .await
        .unwrap()
        .router();

    // The controller's filter does not catch `TodoError`, so the global one handles it.
    let res = get(&router, "/domain/todos/7").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let problem: serde_json::Value = serde_json::from_str(&res.body).unwrap();
    assert_eq!(problem["detail"], "todo 7 not found");

    let res = get(&router, "/domain/conflict").await;
    assert_eq!(res.body, "conflict filter: already exists");

    // Caught, but left as a 500 by the filter.
    assert_eq!(
        get(&router, "/domain/todos/0").await.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );

    // Caught by nobody: the default 500, without the source's details.
    let res = get(&router, "/domain/billing").await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    let problem: serde_json::Value = serde_json::from_str(&res.body).unwrap();
    assert_eq!(problem["detail"], "internal server error");
    assert!(!res.body.contains("4242"));
}

#[tokio::test]
async fn errors_fall_through_to_the_next_filter_that_catches_them() {
    // Within a level, filters are tried in registration order: specific ones go first.
    let router = Hornero::new(DomainApp)
        .filter::<common::domain::TodoErrorFilter>()
        .filter::<GlobalFilter>()
        .build()
        .await
        .unwrap()
        .router();
    assert_eq!(
        get(&router, "/domain/todos/7").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&router, "/domain/billing").await.body,
        "global: internal server error at /domain/billing"
    );

    let catch_all_first = Hornero::new(DomainApp)
        .filter::<GlobalFilter>()
        .filter::<common::domain::TodoErrorFilter>()
        .build()
        .await
        .unwrap()
        .router();
    assert_eq!(
        get(&catch_all_first, "/domain/todos/7").await.body,
        "global: internal server error at /domain/todos/{id}"
    );
}

// --- global enhancers declared by modules (`AppGuard<G>`, Nest's APP_GUARD) ------------------

#[derive(Clone, Default)]
struct GuardTrail(Vec<&'static str>);

fn push_trail(parts: &mut axum::http::request::Parts, label: &'static str) {
    let mut trail = parts.extensions.remove::<GuardTrail>().unwrap_or_default();
    trail.0.push(label);
    parts.extensions.insert(trail);
}

unit_injectable!(BuilderGuard, RootGuard, AppHeader, FeatureController);

impl Guard for BuilderGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        push_trail(ctx.parts, "builder");
        Ok(())
    }
}

impl Guard for RootGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        push_trail(ctx.parts, "root");
        Ok(())
    }
}

/// Private to `TokenModule`: never exported.
struct TokenStore(&'static str);

struct TokenGuard(Arc<TokenStore>);

impl Injectable for TokenGuard {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<TokenStore>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(TokenGuard(r.get()?))
    }
}

impl Guard for TokenGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let expected = format!("Bearer {}", self.0.0);
        if ctx
            .parts
            .headers
            .get("authorization")
            .is_none_or(|value| value != expected.as_str())
        {
            return Err(HttpError::unauthorized("missing token"));
        }
        push_trail(ctx.parts, "token");
        Ok(())
    }
}

impl Interceptor for AppHeader {
    async fn intercept(&self, _: &RouteContext, req: Request, next: Next) -> Response {
        let mut response = next.run(req).await;
        response
            .headers_mut()
            .insert("x-app", HeaderValue::from_static("yes"));
        response
    }
}

labelled_filter!(ModuleFilter => "module");

impl Controller for FeatureController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.get("/feature", "feature", |_: Arc<Self>, Extension(trail): Extension<GuardTrail>| async move {
            trail.0.join(",")
        });
        c.get("/feature/conflict", "conflict", |_: Arc<Self>| async {
            HttpError::conflict("taken")
        });
    }
}

/// Like Nest's `providers: [TokenStore, { provide: APP_GUARD, useClass: TokenGuard }]`.
struct TokenModule;
impl Module for TokenModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide_value(TokenStore("secret"))
            .provide::<AppGuard<TokenGuard>>();
    }
}

/// Knows nothing about tokens, yet its routes are guarded.
struct FeatureModule;
impl Module for FeatureModule {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<FeatureController>();
    }
}

struct GuardedApp;
impl Module for GuardedApp {
    fn define(&self, m: &mut ModuleDef) {
        m.import(TokenModule)
            .import(FeatureModule)
            .provide::<AppGuard<RootGuard>>()
            .provide::<AppInterceptor<AppHeader>>()
            .provide::<AppFilter<ModuleFilter>>();
    }
}

fn authorized(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header("authorization", "Bearer secret")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn modules_declare_global_enhancers_with_app_tokens() {
    let router = Hornero::new(GuardedApp)
        .guard::<BuilderGuard>()
        .build()
        .await
        .unwrap()
        .router();

    // `Hornero` guards first, then imported modules, then the modules importing them.
    let res = send(&router, authorized("/feature")).await;
    assert_eq!(res.body, "builder,token,root");
    assert_eq!(res.headers["x-app"], "yes");

    // `TokenGuard` found the private `TokenStore` in its own module.
    let res = get(&router, "/feature").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.body, "module: missing token at /feature");

    let res = send(&router, authorized("/feature/conflict")).await;
    assert_eq!(res.body, "module: taken at /feature/conflict");
}

#[tokio::test]
async fn filters_registered_on_hornero_are_tried_before_module_declared_ones() {
    let router = Hornero::new(GuardedApp)
        .filter::<GlobalFilter>()
        .build()
        .await
        .unwrap()
        .router();
    assert_eq!(
        send(&router, authorized("/feature/conflict")).await.body,
        "global: taken at /feature/conflict"
    );
}

#[tokio::test]
async fn module_declared_global_guards_can_be_overridden() {
    let router = Hornero::new(GuardedApp)
        .override_guard::<TokenGuard>(AllowAll)
        .build()
        .await
        .unwrap()
        .router();
    assert_eq!(get(&router, "/feature").await.body, "root");
}

#[test]
fn module_declared_global_enhancers_are_validated_where_they_are_declared() {
    // Declared twice in one module.
    struct Twice;
    impl Module for Twice {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<AppGuard<RootGuard>>()
                .provide::<AppGuard<RootGuard>>();
        }
    }
    let err = Hornero::new(Twice).validate().unwrap_err();
    assert!(
        matches!(&err.problems()[0], Problem::DuplicateProvider { provider, .. } if provider == "AppGuard<RootGuard>"),
        "{err}"
    );

    // Resolved in the declaring module, which cannot see `TokenStore`.
    struct Elsewhere;
    impl Module for Elsewhere {
        fn define(&self, m: &mut ModuleDef) {
            m.import(TokenModule).provide::<AppGuard<TokenGuard>>();
        }
    }
    let err = Hornero::new(Elsewhere).validate().unwrap_err();
    assert!(
        matches!(&err.problems()[0], Problem::MissingDependency { dependency, .. } if dependency == "TokenStore"),
        "{err}"
    );
}

#[test]
fn instances_of_one_module_type_cannot_declare_one_global_enhancer() {
    /// Configured with its realm's name.
    struct Realm(&'static str);
    impl Module for Realm {
        fn define(&self, m: &mut ModuleDef) {
            m.provide_value(self.0).provide::<AppGuard<RootGuard>>();
        }

        fn identity(&self) -> Identity {
            Identity::Instance
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Realm("staff")).import(Realm("customers"));
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    insta::assert_snapshot!(err.to_string());

    // Distinct module types declaring it each run it, as before.
    struct Other;
    impl Module for Other {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<AppGuard<RootGuard>>();
        }
    }
    struct Mixed;
    impl Module for Mixed {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Realm("staff")).import(Other);
        }
    }
    Hornero::new(Mixed).validate().unwrap();
}

// --- middleware, layers and mounts ---------------------------------------------------------

struct Banner(&'static str);

struct BannerMiddleware(Arc<Banner>);

impl Injectable for BannerMiddleware {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<Banner>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(BannerMiddleware(r.get()?))
    }
}

impl Middleware for BannerMiddleware {
    async fn handle(&self, req: Request, next: Next) -> Response {
        let mut response = next.run(req).await;
        response
            .headers_mut()
            .insert("x-banner", HeaderValue::from_static(self.0.0));
        response
    }
}

#[tokio::test]
async fn module_middleware_only_wraps_its_own_controllers() {
    struct Banners;
    impl Module for Banners {
        fn define(&self, m: &mut ModuleDef) {
            m.provide_value(Banner("hornero"))
                .middleware::<BannerMiddleware>()
                .layer(axum::middleware::from_fn(
                    |req: Request, next: Next| async move {
                        let mut response = next.run(req).await;
                        response
                            .headers_mut()
                            .insert("x-layer", HeaderValue::from_static("tower"));
                        response
                    },
                ))
                .controller::<PlainController>();
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Banners).import(TodoModule);
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();

    let wrapped = get(&router, "/plain").await;
    assert_eq!(wrapped.headers["x-banner"], "hornero");
    assert_eq!(wrapped.headers["x-layer"], "tower");

    let unwrapped = get(&router, "/todos/1").await;
    assert!(unwrapped.headers.get("x-banner").is_none());
    assert!(unwrapped.headers.get("x-layer").is_none());
}

#[tokio::test]
async fn plain_axum_routers_can_be_mounted() {
    struct Greeting(&'static str);
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide_value(Greeting("hello from DI"))
                .mount_router(
                    "/static",
                    Router::new().route("/ping", axum_get(|| async { "pong" })),
                )
                .mount("/legacy", |greeting: Arc<Greeting>| async move {
                    let text = greeting.0;
                    Ok::<_, BoxError>(
                        Router::new().route("/hello", axum_get(move || async move { text })),
                    )
                });
        }
    }
    let router = Hornero::new(App)
        .global_prefix("/api")
        .build()
        .await
        .unwrap()
        .router();
    assert_eq!(get(&router, "/api/static/ping").await.body, "pong");
    assert_eq!(
        get(&router, "/api/legacy/hello").await.body,
        "hello from DI"
    );
}

#[tokio::test]
async fn router_conflicts_are_bootstrap_errors() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(TodoModule).mount_router(
                "/todos",
                Router::new().route("/{*rest}", axum_get(|| async { "" })),
            );
        }
    }
    let err = Hornero::new(App).build().await.err().unwrap().to_string();
    assert!(err.contains("the router was rejected by axum"), "{err}");
}

#[test]
fn validate_finds_the_conflicts_axum_rejects() {
    struct Conflicting;
    impl Module for Conflicting {
        fn define(&self, m: &mut ModuleDef) {
            m.import(TodoModule).mount_router(
                "/todos",
                Router::new().route("/{*rest}", axum_get(|| async { "" })),
            );
        }
    }
    let err = Hornero::new(Conflicting)
        .validate()
        .unwrap_err()
        .to_string();
    assert!(err.contains("the router was rejected by axum"), "{err}");

    // Mounts that do not overlap with anything are fine, with or without a prefix.
    struct Fine;
    impl Module for Fine {
        fn define(&self, m: &mut ModuleDef) {
            m.import(TodoModule)
                .mount_router(
                    "/static",
                    Router::new().route("/ping", axum_get(|| async { "pong" })),
                )
                .mount(
                    "/legacy",
                    || async move { Ok::<_, BoxError>(Router::new()) },
                );
        }
    }
    Hornero::new(Fine).validate().unwrap();
    Hornero::new(Fine).global_prefix("/api").validate().unwrap();
}

// --- lifecycle -----------------------------------------------------------------------------

static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

macro_rules! hooked {
    ($name:ident $(: $dep:ident)?) => {
        struct $name;
        impl Injectable for $name {
            const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
            fn dependencies() -> Vec<Key> { vec![$(Key::of::<$dep>())?] }
            async fn construct(_: &Resolver) -> Result<Self, BoxError> { Ok($name) }
        }
        impl Lifecycle for $name {
            async fn on_module_init(&self) -> Result<(), BoxError> {
                EVENTS.lock().unwrap().push(concat!(stringify!($name), " init").into());
                Ok(())
            }
            async fn on_application_bootstrap(&self) -> Result<(), BoxError> {
                EVENTS.lock().unwrap().push(concat!(stringify!($name), " bootstrap").into());
                Ok(())
            }
            async fn on_module_destroy(&self) -> Result<(), BoxError> {
                EVENTS.lock().unwrap().push(concat!(stringify!($name), " destroy").into());
                Ok(())
            }
            async fn on_application_shutdown(&self) -> Result<(), BoxError> {
                EVENTS.lock().unwrap().push(concat!(stringify!($name), " shutdown").into());
                Ok(())
            }
        }
    };
}

hooked!(Pool);
hooked!(Repository: Pool);

struct LifecycleModule;

impl Module for LifecycleModule {
    fn define(&self, m: &mut ModuleDef) {
        // Registered dependents-first on purpose: hooks follow dependency order, not registration.
        m.provide::<Repository>()
            .provide::<Pool>()
            .controller::<PlainController>();
    }
}

/// Both lifecycle tests share `EVENTS`, so they run inside one test.
#[tokio::test]
async fn lifecycle_hooks_run_in_dependency_order() {
    EVENTS.lock().unwrap().clear();
    let app = Hornero::new(LifecycleModule).build().await.unwrap();
    assert_eq!(
        *EVENTS.lock().unwrap(),
        [
            "Pool init",
            "Repository init",
            "Pool bootstrap",
            "Repository bootstrap"
        ]
    );

    EVENTS.lock().unwrap().clear();
    let shutdown = app.shutdown_handle();
    app.close().await.unwrap();
    assert!(shutdown.is_triggered());
    assert_eq!(
        *EVENTS.lock().unwrap(),
        [
            "Repository destroy",
            "Pool destroy",
            "Repository shutdown",
            "Pool shutdown"
        ]
    );

    // A real server: serve until the shutdown handle fires, then run the hooks.
    EVENTS.lock().unwrap().clear();
    let app = Hornero::new(LifecycleModule).build().await.unwrap();
    let shutdown = app.shutdown_handle();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(app.serve(listener));

    let response = raw_http_get(address, "/plain").await;
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");

    shutdown.trigger();
    server.await.unwrap().unwrap();
    let events = EVENTS.lock().unwrap().clone();
    assert!(events.ends_with(&[
        "Repository destroy".to_string(),
        "Pool destroy".to_string(),
        "Repository shutdown".to_string(),
        "Pool shutdown".to_string(),
    ]));
}

async fn raw_http_get(address: std::net::SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn failing_hooks_abort_the_bootstrap() {
    struct Exploding;
    impl Injectable for Exploding {
        const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Ok(Exploding)
        }
    }
    impl Lifecycle for Exploding {
        async fn on_module_init(&self) -> Result<(), BoxError> {
            Err("migrations failed".into())
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Exploding>();
        }
    }
    let err = Hornero::new(App).build().await.err().unwrap();
    insta::assert_snapshot!(err.to_string());
}

// --- enhancer resolution --------------------------------------------------------------------

unit_injectable!(PassGuard, PassController);

impl Guard for PassGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

impl Controller for PassController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.guard::<PassGuard>();
        c.get("/pass", "pass", |_: Arc<Self>| async { "ok" });
    }
}

struct PassModuleA;
impl Module for PassModuleA {
    fn define(&self, m: &mut ModuleDef) {
        m.provide::<PassGuard>().export::<PassGuard>();
    }
}

struct PassModuleB;
impl Module for PassModuleB {
    fn define(&self, m: &mut ModuleDef) {
        m.provide::<PassGuard>().export::<PassGuard>();
    }
}

#[test]
fn ambiguous_enhancers_are_reported_like_dependencies() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(PassModuleA)
                .import(PassModuleB)
                .controller::<PassController>();
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn ambiguous_global_enhancers_are_reported_in_the_root_module() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(PassModuleA).import(PassModuleB);
        }
    }
    let err = Hornero::new(App)
        .guard::<PassGuard>()
        .validate()
        .unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    assert!(
        matches!(err.problems()[0], Problem::AmbiguousDependency { .. }),
        "{err}"
    );
}

#[test]
fn ambiguous_module_declared_enhancers_are_reported() {
    struct Feature;
    impl Module for Feature {
        fn define(&self, m: &mut ModuleDef) {
            m.import(PassModuleA)
                .import(PassModuleB)
                .provide::<AppGuard<PassGuard>>();
        }
    }
    let err = Hornero::new(Feature).validate().unwrap_err();
    assert_eq!(err.problems().len(), 1, "{err}");
    assert!(
        matches!(err.problems()[0], Problem::AmbiguousDependency { .. }),
        "{err}"
    );
}

#[test]
fn an_overridden_ambiguous_enhancer_needs_no_resolution() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(PassModuleA)
                .import(PassModuleB)
                .controller::<PassController>();
        }
    }
    Hornero::new(App)
        .override_guard::<PassGuard>(AllowAll)
        .validate()
        .unwrap();
}

// --- http errors ----------------------------------------------------------------------------

#[test]
fn http_error_headers_travel_with_the_error() {
    use hornero_core::HttpError;

    // 401 defaults to a Bearer challenge, available both on the error and on its rendering.
    let err = HttpError::unauthorized("nope");
    assert_eq!(err.headers()["www-authenticate"], "Bearer");
    assert_eq!(err.render().headers()["www-authenticate"], "Bearer");

    // `with_header` replaces the default; other statuses carry no challenge.
    let err = err.with_header(
        axum::http::header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"hornero\""),
    );
    assert_eq!(
        err.render().headers()["www-authenticate"],
        "Basic realm=\"hornero\""
    );
    assert_eq!(
        err.render()
            .headers()
            .get_all("www-authenticate")
            .iter()
            .count(),
        1
    );
    assert!(
        HttpError::forbidden("x")
            .render()
            .headers()
            .get("www-authenticate")
            .is_none()
    );
}

#[test]
fn unserializable_details_are_dropped_not_fatal() {
    use hornero_core::HttpError;
    use std::collections::HashMap;

    // JSON object keys must be strings, so this fails to serialize.
    let bad: HashMap<(u8, u8), u8> = HashMap::from([((1, 2), 3)]);
    let err = HttpError::bad_request("x").with_details(bad);
    assert!(err.details().is_none());
}

// --- failed bootstraps ---------------------------------------------------------------------

/// Declares a module with a `Pool` that records its hooks in statics and keeps the `Shutdown` it
/// was injected with, so a test can check what a failed bootstrap left behind.
macro_rules! probe {
    ($module:ident) => {
        mod $module {
            use super::*;
            use std::sync::atomic::AtomicBool;

            pub static INIT: AtomicBool = AtomicBool::new(false);
            pub static DESTROYED: AtomicBool = AtomicBool::new(false);
            pub static SHUT_DOWN: AtomicBool = AtomicBool::new(false);
            pub static SHUTDOWN: Mutex<Option<Shutdown>> = Mutex::new(None);

            pub fn flag(flag: &AtomicBool) -> bool {
                flag.load(Ordering::SeqCst)
            }

            #[allow(dead_code)]
            pub fn shutdown_triggered() -> bool {
                SHUTDOWN.lock().unwrap().as_ref().unwrap().is_triggered()
            }

            pub struct Pool;
            impl Injectable for Pool {
                const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
                fn dependencies() -> Vec<Key> {
                    vec![Key::of::<Shutdown>()]
                }
                async fn construct(r: &Resolver) -> Result<Self, BoxError> {
                    *SHUTDOWN.lock().unwrap() = Some((*r.get::<Shutdown>()?).clone());
                    Ok(Pool)
                }
            }
            impl Lifecycle for Pool {
                async fn on_module_init(&self) -> Result<(), BoxError> {
                    INIT.store(true, Ordering::SeqCst);
                    Ok(())
                }
                async fn on_module_destroy(&self) -> Result<(), BoxError> {
                    DESTROYED.store(true, Ordering::SeqCst);
                    Ok(())
                }
                async fn on_application_shutdown(&self) -> Result<(), BoxError> {
                    SHUT_DOWN.store(true, Ordering::SeqCst);
                    Ok(())
                }
            }
        }
    };
}

probe!(construction);
probe!(init);
probe!(conflict);

#[tokio::test]
async fn a_failed_construction_cleans_up_the_singletons_built_so_far() {
    use construction::*;
    struct Broken;
    impl Injectable for Broken {
        fn dependencies() -> Vec<Key> {
            vec![Key::of::<Pool>()]
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Err("cannot connect".into())
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>().provide::<Broken>();
        }
    }
    let err = Hornero::new(App).build().await.err().unwrap().to_string();
    assert!(err.contains("cannot connect"), "{err}");
    assert!(!flag(&INIT));
    assert!(flag(&DESTROYED) && flag(&SHUT_DOWN));
    assert!(shutdown_triggered());
}

#[tokio::test]
async fn a_failed_init_hook_cleans_up_and_keeps_cleanup_failures() {
    use init::*;
    struct Broken;
    impl Injectable for Broken {
        const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
        fn dependencies() -> Vec<Key> {
            vec![Key::of::<Pool>()]
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Ok(Broken)
        }
    }
    impl Lifecycle for Broken {
        async fn on_application_bootstrap(&self) -> Result<(), BoxError> {
            Err("warm-up failed".into())
        }
        async fn on_module_destroy(&self) -> Result<(), BoxError> {
            Err("destroy failed".into())
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>().provide::<Broken>();
        }
    }
    let err = Hornero::new(App).build().await.err().unwrap();
    let text = err.to_string();
    // The original problem comes first, the cleanup failure after it; nothing is lost.
    let original = text.find("warm-up failed").expect(&text);
    let cleanup = text.find("destroy failed").expect(&text);
    assert!(original < cleanup, "{text}");
    assert_eq!(err.problems().len(), 2);
    // A failing hook does not stop the others from running.
    assert!(flag(&INIT) && flag(&DESTROYED) && flag(&SHUT_DOWN));
    assert!(shutdown_triggered());
}

#[tokio::test]
async fn a_router_conflict_is_found_before_any_hook_runs() {
    use conflict::*;
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(TodoModule).provide::<Pool>().mount_router(
                "/todos",
                Router::new().route("/{*rest}", axum_get(|| async { "" })),
            );
        }
    }
    let err = Hornero::new(App).build().await.err().unwrap().to_string();
    assert!(err.contains("the router was rejected by axum"), "{err}");
    assert!(!flag(&INIT));
    assert!(flag(&DESTROYED) && flag(&SHUT_DOWN));
    assert!(shutdown_triggered());
}

#[tokio::test]
async fn shutdown_failures_are_not_reported_as_boot_failures() {
    struct Leaky;
    impl Injectable for Leaky {
        const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Ok(Leaky)
        }
    }
    impl Lifecycle for Leaky {
        async fn on_application_shutdown(&self) -> Result<(), BoxError> {
            Err("pool did not close".into())
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Leaky>();
        }
    }
    let err = Hornero::new(App)
        .build()
        .await
        .unwrap()
        .close()
        .await
        .unwrap_err();
    assert_eq!(err.problems().len(), 1);
    let text = err.to_string();
    assert!(
        text.starts_with("failed to shut down application: 1 problem"),
        "{text}"
    );
    assert!(text.contains("pool did not close"), "{text}");
}

#[tokio::test]
async fn serve_provides_connect_info() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.mount_router(
                "/",
                Router::new().route(
                    "/peer",
                    axum_get(|ConnectInfo(addr): ConnectInfo<SocketAddr>| async move {
                        addr.ip().to_string()
                    }),
                ),
            );
        }
    }
    let app = Hornero::new(App).build().await.unwrap();
    let shutdown = app.shutdown_handle();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(app.serve(listener));
    let response = raw_http_get(address, "/peer").await;
    assert!(
        response.starts_with("HTTP/1.1 200") && response.ends_with("127.0.0.1"),
        "{response}"
    );
    shutdown.trigger();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn panic_responses_pass_through_global_layers() {
    let router = Hornero::new(TodoModule)
        .layer(axum::middleware::from_fn(
            |req: Request, next: Next| async move {
                let mut res = next.run(req).await;
                res.headers_mut()
                    .insert("x-layer", HeaderValue::from_static("1"));
                res
            },
        ))
        .build()
        .await
        .unwrap()
        .router();
    for path in ["/todos/1", "/todos/panic", "/nowhere"] {
        let res = get(&router, path).await;
        assert_eq!(
            res.headers.get("x-layer").map(HeaderValue::as_bytes),
            Some(&b"1"[..]),
            "{path}"
        );
    }
    assert_eq!(
        get(&router, "/todos/panic").await.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

probe!(timeout);

#[tokio::test]
async fn serve_gives_up_on_stuck_requests_after_the_shutdown_timeout() {
    use timeout::*;
    use tokio::io::AsyncWriteExt;
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>().mount_router(
                "/",
                Router::new().route("/hang", axum_get(std::future::pending::<&'static str>)),
            );
        }
    }
    let app = Hornero::new(App)
        .shutdown_timeout(std::time::Duration::from_millis(200))
        .build()
        .await
        .unwrap();
    let shutdown = app.shutdown_handle();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(app.serve(listener));

    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /hang HTTP/1.1\r\nhost: test\r\n\r\n")
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let started = std::time::Instant::now();
    shutdown.trigger();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("serve returns despite the stuck request")
        .unwrap()
        .unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(150));
    assert!(flag(&DESTROYED) && flag(&SHUT_DOWN));
}

#[tokio::test]
async fn requests_still_running_at_the_shutdown_timeout_are_cancelled_before_the_hooks() {
    use std::convert::Infallible;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
    static IN_FLIGHT_AT_DESTROY: Mutex<Option<usize>> = Mutex::new(None);

    /// Counts a request as in flight until the future or body holding it is dropped.
    struct InFlight;
    impl InFlight {
        fn start() -> Self {
            IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
            InFlight
        }
    }
    impl Drop for InFlight {
        fn drop(&mut self) {
            IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// A streaming body that never produces a frame, like an idle server-sent events stream.
    struct Endless(#[allow(dead_code)] InFlight);
    impl http_body::Body for Endless {
        type Data = axum::body::Bytes;
        type Error = Infallible;
        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Infallible>>> {
            Poll::Pending
        }
    }

    struct Pool;
    impl Injectable for Pool {
        const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Ok(Pool)
        }
    }
    impl Lifecycle for Pool {
        async fn on_module_destroy(&self) -> Result<(), BoxError> {
            *IN_FLIGHT_AT_DESTROY.lock().unwrap() = Some(IN_FLIGHT.load(Ordering::SeqCst));
            Ok(())
        }
    }

    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>().mount_router(
                "/",
                Router::new()
                    .route(
                        "/hang",
                        axum_get(|| async {
                            let _in_flight = InFlight::start();
                            std::future::pending::<&'static str>().await
                        }),
                    )
                    .route(
                        "/events",
                        axum_get(|| async { Body::new(Endless(InFlight::start())) }),
                    ),
            );
        }
    }
    let app = Hornero::new(App)
        .shutdown_timeout(std::time::Duration::from_millis(100))
        .build()
        .await
        .unwrap();
    let shutdown = app.shutdown_handle();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(app.serve(listener));

    let mut clients = Vec::new();
    for path in ["/hang", "/events"] {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nhost: test\r\n\r\n").as_bytes())
            .await
            .unwrap();
        clients.push(stream);
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while IN_FLIGHT.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both requests reach their handlers");

    shutdown.trigger();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("serve returns despite the stuck requests")
        .unwrap()
        .unwrap();
    assert_eq!(
        *IN_FLIGHT_AT_DESTROY.lock().unwrap(),
        Some(0),
        "no request outlives the shutdown timeout into the destroy hooks"
    );

    // The connections are closed: the handler's client got a 503, the stream was cut off.
    let mut responses = Vec::new();
    for mut stream in clients {
        let mut bytes = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_to_end(&mut bytes),
        )
        .await
        .expect("the connection was closed")
        .ok();
        responses.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    assert!(responses[0].starts_with("HTTP/1.1 503"), "{}", responses[0]);
    assert!(
        !responses[1].contains("503"),
        "the stream had already answered 200: {}",
        responses[1]
    );
}

/// Serves `app`, sends `GET {path}` on a connection of its own for each of `paths`, and shuts the
/// application down once `started` reaches `paths.len()`. The clients keep reading meanwhile;
/// returns the start of what each one read before its connection closed.
async fn shut_down_with_requests_in_flight(
    app: hornero_core::App,
    paths: &[&str],
    started: &AtomicUsize,
) -> Vec<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    let shutdown = app.shutdown_handle();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(app.serve(listener));

    let mut readers = Vec::new();
    for path in paths {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nhost: test\r\n\r\n").as_bytes())
            .await
            .unwrap();
        readers.push(tokio::spawn(async move {
            // Keep reading, so an endless body is never held up by a full socket buffer.
            let (mut kept, mut buf) = (Vec::new(), [0; 8192]);
            while let Ok(n @ 1..) = stream.read(&mut buf).await {
                if kept.len() < 4096 {
                    kept.extend_from_slice(&buf[..n]);
                }
            }
            String::from_utf8_lossy(&kept).into_owned()
        }));
    }
    tokio::time::timeout(TIMEOUT, async {
        while started.load(Ordering::SeqCst) < paths.len() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("every request is in flight");

    shutdown.trigger();
    tokio::time::timeout(TIMEOUT, server)
        .await
        .expect("serve returns despite the requests in flight")
        .unwrap()
        .unwrap();
    let mut responses = Vec::new();
    for reader in readers {
        let response = tokio::time::timeout(TIMEOUT, reader)
            .await
            .expect("the connection was closed")
            .unwrap();
        responses.push(response);
    }
    responses
}

/// Statics for one shutdown test: a request holds a `Release` until it is dropped, and `Pool`
/// records whether that had happened by the time its `on_module_destroy` ran.
macro_rules! release_probe {
    ($module:ident) => {
        mod $module {
            use super::*;
            use std::sync::atomic::AtomicBool;

            pub static STARTED: AtomicUsize = AtomicUsize::new(0);
            static RELEASED: AtomicBool = AtomicBool::new(false);
            static RELEASED_AT_DESTROY: Mutex<Option<bool>> = Mutex::new(None);

            pub struct Release;
            impl Release {
                pub fn start() -> Self {
                    STARTED.fetch_add(1, Ordering::SeqCst);
                    Release
                }
            }
            impl Drop for Release {
                fn drop(&mut self) {
                    RELEASED.store(true, Ordering::SeqCst);
                }
            }

            pub fn released_before_destroy() -> bool {
                *RELEASED_AT_DESTROY.lock().unwrap() == Some(true)
            }

            pub struct Pool;
            impl Injectable for Pool {
                const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
                fn dependencies() -> Vec<Key> {
                    Vec::new()
                }
                async fn construct(_: &Resolver) -> Result<Self, BoxError> {
                    Ok(Pool)
                }
            }
            impl Lifecycle for Pool {
                async fn on_module_destroy(&self) -> Result<(), BoxError> {
                    *RELEASED_AT_DESTROY.lock().unwrap() = Some(RELEASED.load(Ordering::SeqCst));
                    Ok(())
                }
            }
        }
    };
}

release_probe!(answered);

#[tokio::test]
async fn a_request_cancelled_at_the_shutdown_timeout_gets_its_503_through_the_global_layers() {
    use answered::*;
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>().mount_router(
                "/",
                Router::new().route(
                    "/hang",
                    axum_get(|| async {
                        let _release = Release::start();
                        std::future::pending::<&'static str>().await
                    }),
                ),
            );
        }
    }
    let app = Hornero::new(App)
        .layer(tag!("global"))
        .shutdown_timeout(std::time::Duration::from_millis(100))
        .build()
        .await
        .unwrap();
    let responses = shut_down_with_requests_in_flight(app, &["/hang"], &STARTED).await;

    let response = responses[0].to_ascii_lowercase();
    assert!(response.starts_with("http/1.1 503"), "{response}");
    // Like any other error: through the global layers (CORS headers, say), as problem+json.
    assert!(response.contains("x-global: wrapped"), "{response}");
    assert!(
        response.contains("content-type: application/problem+json"),
        "{response}"
    );
    assert!(
        response.contains("the server is shutting down"),
        "{response}"
    );
    assert!(released_before_destroy());
}

release_probe!(stalled);

#[tokio::test]
async fn a_global_middleware_stuck_before_next_is_cancelled_before_the_hooks() {
    use stalled::*;
    unit_injectable!(Stall);
    impl Middleware for Stall {
        async fn handle(&self, _: Request, _: Next) -> Response {
            let _release = Release::start();
            std::future::pending().await
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>();
        }
    }
    let app = Hornero::new(App)
        .middleware::<Stall>()
        .shutdown_timeout(std::time::Duration::from_millis(100))
        .build()
        .await
        .unwrap();
    let responses = shut_down_with_requests_in_flight(app, &["/anything"], &STARTED).await;
    assert!(responses[0].starts_with("HTTP/1.1 503"), "{}", responses[0]);
    assert!(released_before_destroy());
}

release_probe!(firehose);

#[tokio::test]
async fn a_body_that_is_always_ready_is_still_cut_off_before_the_hooks() {
    use firehose::*;
    use std::convert::Infallible;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// An endless event stream that always has the next event ready.
    struct Firehose(#[allow(dead_code)] Release);
    impl http_body::Body for Firehose {
        type Data = axum::body::Bytes;
        type Error = Infallible;
        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Infallible>>> {
            let event = axum::body::Bytes::from_static(b"data: tick\n\n");
            Poll::Ready(Some(Ok(http_body::Frame::data(event))))
        }
    }

    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>().mount_router(
                "/",
                Router::new().route(
                    "/events",
                    axum_get(|| async { Body::new(Firehose(Release::start())) }),
                ),
            );
        }
    }
    let app = Hornero::new(App)
        .shutdown_timeout(std::time::Duration::from_millis(100))
        .build()
        .await
        .unwrap();
    let responses = shut_down_with_requests_in_flight(app, &["/events"], &STARTED).await;
    assert!(responses[0].starts_with("HTTP/1.1 200"), "{}", responses[0]);
    assert!(released_before_destroy());
}

#[tokio::test]
async fn serve_answers_several_requests_on_one_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let app = Hornero::new(TodoModule).build().await.unwrap();
    let shutdown = app.shutdown_handle();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(app.serve(listener));

    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            b"GET /todos/1 HTTP/1.1\r\nhost: test\r\n\r\n\
              GET /missing HTTP/1.1\r\nhost: test\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut response = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_string(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("write tests"), "{response}");
    assert!(response.contains("HTTP/1.1 404"), "{response}");

    shutdown.trigger();
    server.await.unwrap().unwrap();
}

probe!(early);

#[tokio::test]
async fn serving_an_application_that_is_already_shutting_down_returns_at_once() {
    use early::*;
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>();
        }
    }
    let app = Hornero::new(App).build().await.unwrap();
    app.shutdown_handle().trigger();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), app.serve(listener))
        .await
        .expect("serve does not wait for the default shutdown timeout")
        .unwrap();
    assert!(flag(&DESTROYED) && flag(&SHUT_DOWN));
}

probe!(bind);

#[tokio::test]
async fn listen_closes_the_application_when_the_address_is_taken() {
    use bind::*;
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Pool>();
        }
    }
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let err = Hornero::new(App)
        .listen(taken.local_addr().unwrap())
        .await
        .unwrap_err();
    assert_eq!(
        err.downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::AddrInUse),
        "{err}"
    );
    // The application had started, so it is shut down again like a failed bootstrap.
    assert!(flag(&INIT));
    assert!(flag(&DESTROYED) && flag(&SHUT_DOWN));
    assert!(shutdown_triggered());
}

#[tokio::test]
async fn listen_reports_hooks_that_fail_while_closing_after_a_bind_error() {
    struct Leaky;
    impl Injectable for Leaky {
        const LIFECYCLE: Option<LifecycleFn<Self>> = hooks::<Self>();
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Ok(Leaky)
        }
    }
    impl Lifecycle for Leaky {
        async fn on_module_destroy(&self) -> Result<(), BoxError> {
            Err("pool did not close".into())
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Leaky>();
        }
    }
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let err = Hornero::new(App)
        .listen(taken.local_addr().unwrap())
        .await
        .unwrap_err();

    // Still the bind error first, so callers matching on its kind keep working.
    let io = err
        .downcast_ref::<std::io::Error>()
        .unwrap_or_else(|| panic!("{err}"));
    assert_eq!(io.kind(), std::io::ErrorKind::AddrInUse, "{err}");
    let text = err.to_string();
    assert!(
        text.contains("failed in on_module_destroy: pool did not close"),
        "{text}"
    );
    let cleanup = std::error::Error::source(io)
        .and_then(|source| source.downcast_ref::<hornero_core::ShutdownError>())
        .expect("the hook failure is the source");
    assert_eq!(cleanup.problems().len(), 1);
}

// --- fallbacks and method routing ----------------------------------------------------------

#[tokio::test]
async fn a_router_mounted_at_the_root_keeps_its_fallback() {
    struct Spa;
    impl Module for Spa {
        fn define(&self, m: &mut ModuleDef) {
            m.import(TodoModule).mount_router(
                "/",
                Router::new()
                    .route("/health", axum_get(|| async { "ok" }))
                    .fallback(|| async { "index.html" }),
            );
        }
    }
    let router = Hornero::new(Spa).build().await.unwrap().router();
    // A single-page app: client-side routes get the page, everything else is unchanged.
    assert_eq!(get(&router, "/settings/profile").await.body, "index.html");
    assert_eq!(get(&router, "/health").await.body, "ok");
    assert_eq!(get(&router, "/todos/1").await.status, StatusCode::OK);
    assert_eq!(
        get(&router, "/todos/9").await.json()["detail"],
        "todo 9 not found"
    );
}

#[tokio::test]
async fn only_unmatched_requests_become_the_problem_json_404() {
    unit_injectable!(Lookup);
    impl Controller for Lookup {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/lookup", "lookup", |_: Arc<Self>| async {
                StatusCode::NOT_FOUND
            });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Lookup>()
                .mount_router(
                    "/",
                    Router::new().route("/health", axum_get(|| async { "ok" })),
                )
                .mount_router(
                    "/static",
                    Router::new().route("/ping", axum_get(|| async { "pong" })),
                );
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();

    // A handler's own bare 404 is its answer, not a missing route.
    let res = get(&router, "/lookup").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.body.is_empty() && res.headers.get("content-type").is_none());

    // Mounts without a fallback of their own fall through to hornero's.
    for path in ["/missing", "/static/missing"] {
        let res = get(&router, path).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            res.json()["detail"],
            "no route matches the request",
            "{path}"
        );
    }
}

unit_injectable!(Locked);

impl Middleware for Locked {
    async fn handle(&self, _: Request, _: Next) -> Response {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

#[tokio::test]
async fn unmatched_requests_run_outside_every_module_layer() {
    struct Vault;
    impl Module for Vault {
        fn define(&self, m: &mut ModuleDef) {
            m.middleware::<Locked>()
                .controller::<PlainController>()
                .mount_router(
                    "/",
                    Router::new().route("/health", axum_get(|| async { "ok" })),
                );
        }
    }
    struct Tagged;
    impl Module for Tagged {
        fn define(&self, m: &mut ModuleDef) {
            m.layer(tag!("tagged")).mount_router(
                "/tagged",
                Router::new().route("/ping", axum_get(|| async { "pong" })),
            );
        }
    }
    // Which module's router axum merges last must not matter.
    struct VaultLast;
    impl Module for VaultLast {
        fn define(&self, m: &mut ModuleDef) {
            m.import(TodoModule).import(Tagged).import(Vault);
        }
    }
    struct TaggedLast;
    impl Module for TaggedLast {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Vault).import(TodoModule).import(Tagged);
        }
    }

    for router in [
        Hornero::new(VaultLast).build().await.unwrap().router(),
        Hornero::new(TaggedLast).build().await.unwrap().router(),
    ] {
        let missing = get(&router, "/missing").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(missing.json()["detail"], "no route matches the request");
        assert!(missing.headers.get("x-tagged").is_none());

        // The modules' own routes, their mounts' included, are still wrapped.
        for path in ["/plain", "/health"] {
            assert_eq!(
                get(&router, path).await.status,
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
        assert_eq!(
            get(&router, "/tagged/ping").await.headers["x-tagged"],
            "wrapped"
        );
        assert!(
            get(&router, "/todos/1")
                .await
                .headers
                .get("x-tagged")
                .is_none()
        );
    }
}

#[tokio::test]
async fn a_root_fallback_runs_inside_the_global_layers_only() {
    struct Spa;
    impl Module for Spa {
        fn define(&self, m: &mut ModuleDef) {
            m.layer(tag!("module")).mount_router(
                "/",
                Router::new()
                    .route("/health", axum_get(|| async { "ok" }))
                    .fallback(|| async { "index.html" }),
            );
        }
    }
    let router = Hornero::new(Spa)
        .layer(tag!("global"))
        .build()
        .await
        .unwrap()
        .router();

    let page = get(&router, "/settings/profile").await;
    assert_eq!(page.body, "index.html");
    assert_eq!(page.headers["x-global"], "wrapped");
    assert!(page.headers.get("x-module").is_none());

    let health = get(&router, "/health").await;
    assert_eq!(health.body, "ok");
    assert_eq!(health.headers["x-global"], "wrapped");
    assert_eq!(health.headers["x-module"], "wrapped");
}

#[tokio::test]
async fn a_fallback_mounted_under_a_prefix_runs_inside_its_module() {
    struct Tagged;
    impl Module for Tagged {
        fn define(&self, m: &mut ModuleDef) {
            m.layer(tag!("tagged")).mount_router(
                "/tagged",
                Router::new()
                    .route("/ping", axum_get(|| async { "pong" }))
                    .fallback(|| async { "tagged fallback" }),
            );
        }
    }
    struct Vault;
    impl Module for Vault {
        fn define(&self, m: &mut ModuleDef) {
            m.middleware::<Locked>()
                .mount_router("/vault", Router::new().fallback(|| async { "classified" }));
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(Tagged).import(Vault);
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();

    // Under its prefix, a mount's own fallback answers inside its module's layers and middleware.
    let res = get(&router, "/tagged/missing").await;
    assert_eq!(res.body, "tagged fallback");
    assert_eq!(res.headers["x-tagged"], "wrapped");
    assert_eq!(
        get(&router, "/vault/missing").await.status,
        StatusCode::UNAUTHORIZED
    );

    // Elsewhere it is the application's fallback, outside both.
    let res = get(&router, "/missing").await;
    assert_eq!(res.json()["detail"], "no route matches the request");
    assert!(res.headers.get("x-tagged").is_none());
}

#[tokio::test]
async fn a_path_rewritten_after_routing_cannot_reach_a_mounted_route_around_its_module() {
    struct Vault;
    impl Module for Vault {
        fn define(&self, m: &mut ModuleDef) {
            m.middleware::<Locked>().mount_router(
                "/",
                Router::new()
                    .route("/secret", axum_get(|| async { "classified" }))
                    .fallback(|| async { "index.html" }),
            );
        }
    }
    // A global layer runs after routing: `/alias` is routed to the fallback, then rewritten.
    let router = Hornero::new(Vault)
        .layer(axum::middleware::map_request(
            |mut req: Request| async move {
                if req.uri().path() == "/alias" {
                    *req.uri_mut() = "/secret".parse().unwrap();
                }
                req
            },
        ))
        .build()
        .await
        .unwrap()
        .router();

    assert_eq!(
        get(&router, "/secret").await.status,
        StatusCode::UNAUTHORIZED
    );
    let res = get(&router, "/alias").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert_eq!(res.json()["detail"], "no route matches the request");
}

#[tokio::test]
async fn a_fallback_that_answers_an_error_keeps_it() {
    struct Api;
    impl Module for Api {
        fn define(&self, m: &mut ModuleDef) {
            m.mount_router(
                "/",
                Router::new().fallback(|| async { HttpError::not_found("no such page") }),
            );
        }
    }
    let router = Hornero::new(Api).build().await.unwrap().router();
    let res = get(&router, "/anything").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.json()["detail"], "no such page");
}

#[tokio::test]
async fn one_path_can_be_served_by_several_modules() {
    unit_injectable!(Reader, Writer);
    impl Controller for Reader {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/items", "list", |_: Arc<Self>| async { "list" });
        }
    }
    impl Controller for Writer {
        fn routes(c: &mut ControllerDef<Self>) {
            c.post("/items", "create", |_: Arc<Self>| async { "create" })
                .guard::<DenyGuard>();
        }
    }
    struct ReadModule;
    impl Module for ReadModule {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Reader>();
        }
    }
    struct WriteModule;
    impl Module for WriteModule {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Writer>();
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.import(ReadModule).import(WriteModule);
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();
    assert_eq!(get(&router, "/items").await.body, "list");
    let post = send(
        &router,
        Request::post("/items").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(
        post.status,
        StatusCode::FORBIDDEN,
        "the POST route keeps its guard"
    );
    let put = send(&router, Request::put("/items").body(Body::empty()).unwrap()).await;
    assert_eq!(put.status, StatusCode::METHOD_NOT_ALLOWED);
    let allow = put.headers["allow"].to_str().unwrap();
    assert!(allow.contains("GET") && allow.contains("POST"), "{allow}");
}

#[tokio::test]
async fn head_requests_run_the_guards_of_the_get_route() {
    unit_injectable!(Secret);
    impl Controller for Secret {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/secret", "secret", |_: Arc<Self>| async { "classified" })
                .guard::<DenyGuard>();
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Secret>();
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();
    let res = send(
        &router,
        Request::head("/secret").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.is_empty());
}

// --- scope and override diagnostics ---------------------------------------------------------

#[test]
fn controllers_cannot_be_transient() {
    struct Ephemeral;
    impl Injectable for Ephemeral {
        const SCOPE: Scope = Scope::Transient;
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Ok(Ephemeral)
        }
    }
    impl Controller for Ephemeral {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/ephemeral", "ephemeral", |_: Arc<Self>| async { "ok" });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Ephemeral>();
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[test]
fn unused_enhancer_overrides_name_the_enhancer() {
    let err = Hornero::new(TodoModule)
        .override_guard::<DenyGuard>(AllowAll)
        .override_interceptor::<AppHeader>(AppHeader)
        .override_filter::<GlobalFilter>(GlobalFilter)
        .validate()
        .unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[tokio::test]
async fn transient_enhancers_are_built_at_bootstrap() {
    static BUILDS: AtomicUsize = AtomicUsize::new(0);
    struct Counted;
    impl Injectable for Counted {
        const SCOPE: Scope = Scope::Transient;
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            BUILDS.fetch_add(1, Ordering::SeqCst);
            Ok(Counted)
        }
    }
    impl Guard for Counted {
        async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
            Ok(())
        }
    }
    unit_injectable!(Counting);
    impl Controller for Counting {
        fn routes(c: &mut ControllerDef<Self>) {
            c.guard::<Counted>();
            c.get("/counted", "counted", |_: Arc<Self>| async { "ok" });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<Counting>();
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();
    get(&router, "/counted").await;
    get(&router, "/counted").await;
    assert_eq!(
        BUILDS.load(Ordering::SeqCst),
        1,
        "no request builds a provider"
    );
}

#[tokio::test]
async fn an_unbounded_shutdown_timeout_does_not_overflow() {
    let app = Hornero::new(TodoModule)
        .shutdown_timeout(std::time::Duration::MAX)
        .build()
        .await
        .unwrap();
    let shutdown = app.shutdown_handle();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = tokio::spawn(app.serve(listener));
    shutdown.trigger();
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

// --- extractors that are providers ----------------------------------------------------------

struct Directory;

impl Directory {
    fn find(&self, token: &str) -> Option<String> {
        (token == "ada-token").then(|| "ada".to_owned())
    }
}

struct Caller(Arc<Directory>);

impl Injectable for Caller {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<Directory>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(Self(r.get()?))
    }
}

impl Extractor for Caller {
    type Output = String;

    async fn extract(&self, parts: &mut Parts) -> Result<String, HttpError> {
        parts
            .headers
            .get("x-token")
            .and_then(|value| value.to_str().ok())
            .and_then(|token| self.0.find(token))
            .ok_or_else(|| HttpError::unauthorized("invalid token"))
    }
}

fn with_token(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header("x-token", "ada-token")
        .body(Body::empty())
        .unwrap()
}

unit_injectable!(MeController);

impl Controller for MeController {
    fn routes(c: &mut ControllerDef<Self>) {
        // Nothing but the parameter declares the extractor.
        c.get(
            "/me",
            "me",
            |_: Arc<Self>, Extracted(name): Extracted<Caller>| async move { name },
        );
    }
}

struct MeModule;

impl Module for MeModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provider(ProviderDef::value(Directory))
            .controller::<MeController>();
    }
}

#[tokio::test]
async fn extractors_combine_a_provider_with_the_request() {
    let router = Hornero::new(MeModule).build().await.unwrap().router();

    let res = send(&router, with_token("/me")).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body, "ada");

    let res = get(&router, "/me").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.json()["detail"], "invalid token");
}

#[test]
fn an_extractor_without_its_dependency_fails_at_bootstrap() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<MeController>();
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

#[tokio::test]
async fn extractors_can_be_overridden_with_any_extractor() {
    struct Anyone;
    impl Extractor for Anyone {
        type Output = String;
        async fn extract(&self, _: &mut Parts) -> Result<String, HttpError> {
            Ok("anyone".to_owned())
        }
    }
    // Without `Directory`: the replacement does not need the original's dependencies.
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<MeController>();
        }
    }
    let router = Hornero::new(App)
        .override_extractor::<Caller>(Anyone)
        .build()
        .await
        .unwrap()
        .router();
    assert_eq!(get(&router, "/me").await.body, "anyone");

    let err = Hornero::new(TodoModule)
        .override_extractor::<Caller>(Anyone)
        .validate()
        .unwrap_err();
    assert!(
        matches!(err.problems(), [Problem::UnusedOverride { provider }] if provider == "Caller")
    );
}

#[test]
fn the_route_table_lists_extractors() {
    let routes = Hornero::new(MeModule).routes().unwrap();
    let route = routes.iter().next().unwrap();
    assert_eq!(route.extractors().len(), 1);
    assert!(route.extractors()[0].is::<Caller>());
}

/// Counts its runs, so a test can tell which requests reached it.
#[derive(Default)]
struct Visits(AtomicUsize);

impl Injectable for Visits {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }
    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Ok(Self::default())
    }
}

impl Extractor for Visits {
    type Output = usize;

    async fn extract(&self, _: &mut Parts) -> Result<usize, HttpError> {
        Ok(self.0.fetch_add(1, Ordering::SeqCst) + 1)
    }
}

unit_injectable!(VisitsController);

impl Controller for VisitsController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.get(
            "/counted",
            "counted",
            |_: Arc<Self>, visit: Extracted<Visits>| async move { visit.to_string() },
        );
        c.get("/skipped", "skipped", |_: Arc<Self>| async { "skipped" });
        c.get(
            "/twice",
            "twice",
            |_: Arc<Self>, a: Extracted<Visits>, b: Extracted<Visits>| async move {
                format!("{} {}", a.0, b.0)
            },
        );
    }
}

#[tokio::test]
async fn extractors_run_once_per_parameter_of_the_handlers_that_ask() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Visits>().controller::<VisitsController>();
        }
    }
    let app = Hornero::new(App).build().await.unwrap();
    let router = app.router();

    assert_eq!(get(&router, "/skipped").await.body, "skipped");
    assert_eq!(get(&router, "/counted").await.body, "1");
    assert_eq!(get(&router, "/twice").await.body, "2 3");

    // The route reached the provider the module registered, not a copy of its own.
    assert_eq!(app.get::<Visits>().unwrap().0.load(Ordering::SeqCst), 3);

    let routes = Hornero::new(App).routes().unwrap();
    let twice = routes.iter().find(|route| route.handler() == "twice");
    assert_eq!(twice.unwrap().extractors().len(), 1);
}

struct Stamped;

impl Injectable for Stamped {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }
    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Ok(Self)
    }
}

/// Reads what the guard and the interceptor below left on the request.
impl Extractor for Stamped {
    type Output = String;

    async fn extract(&self, parts: &mut Parts) -> Result<String, HttpError> {
        let guard = parts.extensions.get::<GuardStamp>();
        let interceptor = parts.extensions.get::<InterceptorStamp>();
        match (guard, interceptor) {
            (Some(guard), Some(interceptor)) => Ok(format!("{} {}", guard.0, interceptor.0)),
            _ => Err(HttpError::forbidden("ran before the pipeline")),
        }
    }
}

#[derive(Clone)]
struct GuardStamp(&'static str);
#[derive(Clone)]
struct InterceptorStamp(&'static str);

unit_injectable!(
    StampGuard,
    StampInterceptor,
    StampedController,
    RenderRejection
);

impl Guard for StampGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        ctx.parts.extensions.insert(GuardStamp("guard"));
        Ok(())
    }
}

impl Interceptor for StampInterceptor {
    async fn intercept(&self, _: &RouteContext, mut req: Request, next: Next) -> Response {
        req.extensions_mut().insert(InterceptorStamp("interceptor"));
        next.run(req).await
    }
}

impl ExceptionFilter for RenderRejection {
    async fn catch(&self, err: &HttpError, _: &RouteContext) -> Response {
        (StatusCode::IM_A_TEAPOT, err.message().to_owned()).into_response()
    }
}

impl Controller for StampedController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.get(
            "/stamped",
            "stamped",
            |_: Arc<Self>, stamps: Extracted<Stamped>| async move { stamps.0 },
        )
        .guard::<StampGuard>()
        .interceptor::<StampInterceptor>();
        // No guard or interceptor here, so the extractor rejects.
        c.get(
            "/rejected",
            "rejected",
            |_: Arc<Self>, stamps: Extracted<Stamped>| async move { stamps.0 },
        )
        .filter::<RenderRejection>();
    }
}

struct StampedModule;

impl Module for StampedModule {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<StampedController>();
    }
}

#[tokio::test]
async fn extractors_run_after_guards_and_interceptors() {
    let router = Hornero::new(StampedModule).build().await.unwrap().router();
    assert_eq!(get(&router, "/stamped").await.body, "guard interceptor");
}

#[tokio::test]
async fn extractor_rejections_reach_exception_filters() {
    let router = Hornero::new(StampedModule).build().await.unwrap().router();
    let res = get(&router, "/rejected").await;
    assert_eq!(res.status, StatusCode::IM_A_TEAPOT);
    assert_eq!(res.body, "ran before the pipeline");
}

unit_injectable!(MixedController);

impl Controller for MixedController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.post(
            "/todos/{id}",
            "with_body",
            |_: Arc<Self>,
             Path(id): Path<u32>,
             Extracted(owner): Extracted<Caller>,
             Json(todo): Json<Todo>| async move { format!("{owner} {id} {}", todo.title) },
        );
        c.get(
            "/todos/{id}",
            "last",
            |_: Arc<Self>, Path(id): Path<u32>, Extracted(owner): Extracted<Caller>| async move {
                format!("{owner} {id}")
            },
        );
    }
}

#[tokio::test]
async fn extractors_mix_with_axum_extractors_in_any_position() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provider(ProviderDef::value(Directory))
                .controller::<MixedController>();
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();

    assert_eq!(send(&router, with_token("/todos/7")).await.body, "ada 7");

    let request = Request::post("/todos/7")
        .header("x-token", "ada-token")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"id":7,"title":"write"}"#))
        .unwrap();
    assert_eq!(send(&router, request).await.body, "ada 7 write");
}

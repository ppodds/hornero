mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Extension, Path, Request};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get as axum_get;
use common::domain::DomainApp;
use common::{get, send};
use hornero_core::{
    AppFilter, AppGuard, AppInterceptor, BoxError, Controller, ControllerDef, ExceptionFilter,
    Guard, GuardContext, Hornero, HttpError, Injectable, Interceptor, Key, Lifecycle, LifecycleFn,
    Middleware, Module, ModuleDef, Problem, RequestContext, Resolver, RouteContext, Scope,
    Shutdown, hooks,
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

// --- request scope -------------------------------------------------------------------------

static REQUEST_IDS: AtomicUsize = AtomicUsize::new(0);

struct RequestId(usize);

impl Injectable for RequestId {
    const SCOPE: Scope = Scope::Request;
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }
    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Ok(RequestId(REQUEST_IDS.fetch_add(1, Ordering::SeqCst)))
    }
}

struct Helper(Arc<RequestId>);

impl Injectable for Helper {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<RequestId>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(Helper(r.get()?))
    }
}

/// Pulled into request scope through both of its dependencies.
struct ScopedController(Arc<RequestId>, Arc<Helper>);

impl Injectable for ScopedController {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<RequestId>(), Key::of::<Helper>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(ScopedController(r.get()?, r.get()?))
    }
}

impl Controller for ScopedController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.get("/ids", "ids", |this: Arc<Self>| async move {
            format!("{} {}", this.0.0, this.1.0.0)
        });
    }
}

#[tokio::test]
async fn request_scoped_controllers_are_built_per_request() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<RequestId>()
                .provide::<Helper>()
                .controller::<ScopedController>();
        }
    }
    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(
        app.scope_report().find("ScopedController").unwrap().chain,
        ["ScopedController", "RequestId [request]"]
    );
    let router = app.router();

    let first = get(&router, "/ids").await.body;
    let second = get(&router, "/ids").await.body;
    let parse = |body: &str| -> (usize, usize) {
        let (a, b) = body.split_once(' ').unwrap();
        (a.parse().unwrap(), b.parse().unwrap())
    };
    let (a1, b1) = parse(&first);
    let (a2, b2) = parse(&second);
    assert_eq!(a1, b1, "one RequestId per request");
    assert_eq!(a2, b2, "one RequestId per request");
    assert_ne!(a1, a2, "a new RequestId for every request");
}

/// Built from the request context after the guards ran, so it sees the authenticated user.
struct CurrentUser(Option<User>);

impl Injectable for CurrentUser {
    const SCOPE: Scope = Scope::Request;
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<RequestContext>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        let ctx = r.get::<RequestContext>()?;
        Ok(CurrentUser(ctx.get::<User>().cloned()))
    }
}

struct ProfileController(Arc<CurrentUser>);

impl Injectable for ProfileController {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<CurrentUser>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(ProfileController(r.get()?))
    }
}

impl Controller for ProfileController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.guard::<AuthGuard>();
        c.get("/profile", "profile", |this: Arc<Self>| async move {
            match &this.0.0 {
                Some(user) => format!("{} ({})", user.name, user.role),
                None => "anonymous".into(),
            }
        });
    }
}

#[tokio::test]
async fn request_context_sees_data_written_by_guards() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<CurrentUser>().controller::<ProfileController>();
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();
    let res = send(&router, bearer("/profile", "bob:admin")).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (StatusCode::OK, "bob (admin)")
    );
}

/// A request-scoped guard that runs before `AuthGuard` and records what its context held.
struct TenantGuard(Arc<RequestContext>, Arc<TenantSeen>);

struct TenantSeen(Mutex<Vec<Option<String>>>);

impl Injectable for TenantSeen {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }
    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Ok(TenantSeen(Mutex::new(Vec::new())))
    }
}

impl Injectable for TenantGuard {
    const SCOPE: Scope = Scope::Request;
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<RequestContext>(), Key::of::<TenantSeen>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(TenantGuard(r.get()?, r.get()?))
    }
}

impl Guard for TenantGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let user = self.0.get::<User>().map(|user| user.name.clone());
        self.1.0.lock().unwrap().push(user);
        Ok(())
    }
}

#[tokio::test]
async fn a_context_is_snapshotted_when_its_component_is_built() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<TenantSeen>()
                .provide::<CurrentUser>()
                .controller::<ProfileController>();
        }
    }
    let router = Hornero::new(App)
        .guard::<TenantGuard>()
        .build()
        .await
        .unwrap()
        .router();

    // `TenantGuard` runs before `AuthGuard` and keeps its snapshot; `CurrentUser`, built for the
    // handler, gets a new one that has the user.
    let res = send(&router, bearer("/profile", "alice")).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (StatusCode::OK, "alice (user)")
    );
}

#[tokio::test]
async fn filters_get_a_context_when_a_guard_rejects() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<TenantSeen>()
                .provide::<CurrentUser>()
                .controller::<ProfileController>();
        }
    }
    let router = Hornero::new(App)
        .guard::<TenantGuard>()
        .filter::<ContextFilter>()
        .build()
        .await
        .unwrap()
        .router();
    let res = get(&router, "/profile").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(res.body.ends_with(" at /profile"), "{}", res.body);
}

/// A request-scoped filter that names the user the guards authenticated.
struct UserFilter(Arc<RequestContext>);

impl Injectable for UserFilter {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<RequestContext>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(UserFilter(r.get()?))
    }
}

impl ExceptionFilter for UserFilter {
    async fn catch(&self, err: &HttpError, _: &RouteContext) -> Response {
        let user = self
            .0
            .get::<User>()
            .map_or("anonymous", |user| user.name.as_str());
        (err.status(), format!("{} for {user}", err.message())).into_response()
    }
}

unit_injectable!(BusyController);

impl Controller for BusyController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.guard::<AuthGuard>();
        c.get("/busy", "busy", |_: Arc<Self>| async {
            Err::<String, _>(HttpError::conflict("busy"))
        });
    }
}

#[tokio::test]
async fn filters_see_what_guards_wrote_when_the_handler_fails() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.controller::<BusyController>();
        }
    }
    let router = Hornero::new(App)
        .filter::<UserFilter>()
        .build()
        .await
        .unwrap()
        .router();

    // The filter is built after the handler failed, so it sees the request as the guards left it.
    let res = send(&router, bearer("/busy", "bob")).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (StatusCode::CONFLICT, "busy for bob")
    );
}

/// A guard that is itself request-scoped.
struct RequestGuard(Arc<RequestId>);

impl Injectable for RequestGuard {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<RequestId>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(RequestGuard(r.get()?))
    }
}

impl Guard for RequestGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        ctx.parts.extensions.insert(GuardSaw(self.0.0));
        Ok(())
    }
}

#[derive(Clone)]
struct GuardSaw(usize);

#[tokio::test]
async fn request_scoped_guards_share_the_request_scope() {
    unit_injectable!(Plain);
    struct Shared(Arc<RequestId>);
    impl Injectable for Shared {
        fn dependencies() -> Vec<Key> {
            vec![Key::of::<RequestId>()]
        }
        async fn construct(r: &Resolver) -> Result<Self, BoxError> {
            Ok(Shared(r.get()?))
        }
    }
    impl Controller for Shared {
        fn routes(c: &mut ControllerDef<Self>) {
            c.guard::<RequestGuard>();
            c.get(
                "/shared",
                "shared",
                |this: Arc<Self>, Extension(saw): Extension<GuardSaw>| async move {
                    (this.0.0 == saw.0).to_string()
                },
            );
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<RequestId>()
                .provide::<Plain>()
                .controller::<Shared>();
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();
    assert_eq!(get(&router, "/shared").await.body, "true");
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

#[test]
fn request_scoped_middleware_is_rejected() {
    struct ScopedMiddleware(#[allow(dead_code)] Arc<RequestId>);
    impl Injectable for ScopedMiddleware {
        fn dependencies() -> Vec<Key> {
            vec![Key::of::<RequestId>()]
        }
        async fn construct(r: &Resolver) -> Result<Self, BoxError> {
            Ok(ScopedMiddleware(r.get()?))
        }
    }
    impl Middleware for ScopedMiddleware {
        async fn handle(&self, req: Request, next: Next) -> Response {
            next.run(req).await
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<RequestId>().middleware::<ScopedMiddleware>();
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
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

#[tokio::test]
async fn failing_request_scoped_providers_become_500s() {
    struct Flaky;
    impl Injectable for Flaky {
        const SCOPE: Scope = Scope::Request;
        fn dependencies() -> Vec<Key> {
            Vec::new()
        }
        async fn construct(_: &Resolver) -> Result<Self, BoxError> {
            Err("upstream unavailable".into())
        }
    }
    struct FlakyController(#[allow(dead_code)] Arc<Flaky>);
    impl Injectable for FlakyController {
        fn dependencies() -> Vec<Key> {
            vec![Key::of::<Flaky>()]
        }
        async fn construct(r: &Resolver) -> Result<Self, BoxError> {
            Ok(FlakyController(r.get()?))
        }
    }
    impl Controller for FlakyController {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/flaky", "flaky", |_: Arc<Self>| async { "unreachable" });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Flaky>().controller::<FlakyController>();
        }
    }
    let router = Hornero::new(App)
        .filter::<GlobalFilter>()
        .build()
        .await
        .unwrap()
        .router();
    let res = get(&router, "/flaky").await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        res.body,
        "global: failed to construct a request-scoped component at /flaky"
    );
    assert!(!res.body.contains("upstream unavailable"));
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

/// A request-scoped filter that reads the request context.
struct ContextFilter(Arc<RequestContext>);

impl Injectable for ContextFilter {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<RequestContext>()]
    }
    async fn construct(r: &Resolver) -> Result<Self, BoxError> {
        Ok(ContextFilter(r.get()?))
    }
}

impl ExceptionFilter for ContextFilter {
    async fn catch(&self, err: &HttpError, _: &RouteContext) -> Response {
        (
            err.status(),
            format!("{} at {}", err.message(), self.0.uri().path()),
        )
            .into_response()
    }
}

#[tokio::test]
async fn request_scoped_filters_do_not_hide_guard_data_from_the_handler() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<CurrentUser>().controller::<ProfileController>();
        }
    }
    let router = Hornero::new(App)
        .filter::<ContextFilter>()
        .build()
        .await
        .unwrap()
        .router();

    // The filter captures nothing up front, so the handler's `CurrentUser` sees the guard's user.
    let res = send(&router, bearer("/profile", "bob:admin")).await;
    assert_eq!(
        (res.status, res.body.as_str()),
        (StatusCode::OK, "bob (admin)")
    );

    // The filter still gets a context when it has to render a guard rejection.
    let res = get(&router, "/profile").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(res.body.ends_with(" at /profile"), "{}", res.body);
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
fn mounts_cannot_be_request_scoped() {
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.mount("/whoami", |_: Arc<RequestContext>| async {
                Ok::<_, BoxError>(Router::new())
            });
        }
    }
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("mount /whoami (in App) cannot be request-scoped"),
        "{err}"
    );
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
async fn transient_providers_are_fresh_within_one_request() {
    service!(Nonce; scope = Scope::Transient);
    service!(Shared; scope = Scope::Request);
    service!(First: Nonce, Shared; scope = Scope::Request);
    service!(Second: Nonce, Shared; scope = Scope::Request);
    service!(Pairing: First, Second);
    impl Controller for Pairing {
        fn routes(c: &mut ControllerDef<Self>) {
            c.get("/pairing", "pairing", |this: Arc<Self>| async move {
                format!(
                    "nonce shared: {}, scoped shared: {}",
                    Arc::ptr_eq(&this.0.0, &this.1.0),
                    Arc::ptr_eq(&this.0.1, &this.1.1)
                )
            });
        }
    }
    struct App;
    impl Module for App {
        fn define(&self, m: &mut ModuleDef) {
            m.provide::<Nonce>()
                .provide::<Shared>()
                .provide::<First>()
                .provide::<Second>()
                .controller::<Pairing>();
        }
    }
    let router = Hornero::new(App).build().await.unwrap().router();
    assert_eq!(
        get(&router, "/pairing").await.body,
        "nonce shared: false, scoped shared: true"
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

//! An application written entirely with the macros (L2), exercised end to end.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use hornero::prelude::*;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

// --- providers -----------------------------------------------------------------------------

pub struct AppConfig {
    greeting: &'static str,
}

pub trait Clock: Send + Sync {
    fn now(&self) -> u64;
}

#[derive(Injectable)]
pub struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> u64 {
        1_700_000_000
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct Note {
    id: u32,
    text: String,
    created_at: u64,
}

#[derive(Deserialize)]
pub struct NewNote {
    text: String,
}

#[derive(Injectable)]
pub struct NoteStore {
    clock: Arc<dyn Clock>,
    #[inject(default)]
    notes: Mutex<HashMap<u32, Note>>,
    #[inject(with = AtomicUsize::new(1))]
    next_id: AtomicUsize,
}

impl NoteStore {
    fn insert(&self, text: String) -> Note {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as u32;
        let note = Note {
            id,
            text,
            created_at: self.clock.now(),
        };
        self.notes.lock().unwrap().insert(id, note.clone());
        note
    }
}

/// Constructor injection with an async, fallible constructor.
pub struct Greeter {
    greeting: String,
}

#[injectable]
impl Greeter {
    async fn new(config: Arc<AppConfig>) -> Result<Self, std::io::Error> {
        Ok(Self {
            greeting: config.greeting.to_string(),
        })
    }

    fn greet(&self, name: &str) -> String {
        format!("{}, {name}", self.greeting)
    }
}

/// A `#[constructor]` that is not called `new`, built by a factory-style dependency.
pub struct Shouter(Arc<Greeter>);

#[injectable]
impl Shouter {
    #[constructor]
    fn build(greeter: Arc<Greeter>) -> Self {
        Self(greeter)
    }

    fn shout(&self, name: &str) -> String {
        self.0.greet(name).to_uppercase()
    }
}

static REQUESTS: AtomicUsize = AtomicUsize::new(0);

/// Request-scoped through the struct form of `#[injectable]`.
#[injectable(scope = request)]
pub struct RequestStamp {
    #[inject(with = REQUESTS.fetch_add(1, Ordering::SeqCst))]
    number: usize,
}

pub struct Counter(AtomicUsize);

// --- enhancers -----------------------------------------------------------------------------

#[derive(Clone)]
pub struct Caller(String);

#[derive(Injectable)]
pub struct ApiKeyGuard;

impl Guard for ApiKeyGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        if ctx.route.meta::<Public>().is_some() {
            return Ok(());
        }
        let key = ctx
            .parts
            .headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| HttpError::unauthorized("missing api key"))?;
        ctx.parts.extensions.insert(Caller(key.to_string()));
        Ok(())
    }
}

pub struct Public;
pub struct Audited(&'static str);

#[derive(Injectable)]
pub struct AuditInterceptor;

impl Interceptor for AuditInterceptor {
    async fn intercept(&self, ctx: &RouteContext, req: Request, next: Next) -> Response {
        let mut response = next.run(req).await;
        if let Some(Audited(label)) = ctx.meta::<Audited>() {
            response
                .headers_mut()
                .insert("x-audited", HeaderValue::from_static(label));
        }
        response
    }
}

#[derive(Injectable)]
pub struct PlainTextErrors;

impl ExceptionFilter for PlainTextErrors {
    async fn catch(&self, err: &HttpError, _: &RouteContext) -> Response {
        (err.status(), format!("error: {}", err.message())).into_response()
    }
}

#[derive(Injectable)]
pub struct PoweredBy {
    counter: Arc<Counter>,
}

impl Middleware for PoweredBy {
    async fn handle(&self, req: Request, next: Next) -> Response {
        self.counter.0.fetch_add(1, Ordering::SeqCst);
        let mut response = next.run(req).await;
        response
            .headers_mut()
            .insert("x-powered-by", HeaderValue::from_static("hornero"));
        response
    }
}

// --- controllers ---------------------------------------------------------------------------

#[controller("/notes")]
#[guards(ApiKeyGuard)]
#[interceptors(AuditInterceptor)]
#[filters(PlainTextErrors)]
pub struct NoteController {
    store: Arc<NoteStore>,
    shouter: Arc<Shouter>,
}

#[derive(Deserialize)]
pub struct Shout {
    loud: Option<bool>,
}

#[routes]
impl NoteController {
    #[post("/")]
    #[meta(Audited("create"))]
    async fn create(
        &self,
        Extension(caller): Extension<Caller>,
        Json(input): Json<NewNote>,
    ) -> (StatusCode, Json<Note>) {
        let note = self
            .store
            .insert(format!("{} (by {})", input.text, caller.0));
        (StatusCode::CREATED, Json(note))
    }

    #[get("/{id}")]
    async fn find_one(&self, Path(id): Path<u32>) -> Result<Json<Note>, HttpError> {
        self.store
            .notes
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .map(Json)
            .ok_or_else(|| HttpError::not_found(format!("note {id} not found")))
    }

    #[get("/hello/{name}")]
    #[meta(Public)]
    fn hello(&self, Path(name): Path<String>, Query(shout): Query<Shout>) -> String {
        if shout.loud.unwrap_or(false) {
            self.shouter.shout(&name)
        } else {
            self.shouter.0.greet(&name)
        }
    }

    #[delete("/{id}")]
    async fn remove(self: Arc<Self>, Path(id): Path<u32>) -> StatusCode {
        match self.store.notes.lock().unwrap().remove(&id) {
            Some(_) => StatusCode::NO_CONTENT,
            None => StatusCode::NOT_FOUND,
        }
    }

    #[route("TRACE", "/")]
    #[meta(Public)]
    async fn purge(&self) -> StatusCode {
        self.store.notes.lock().unwrap().clear();
        StatusCode::RESET_CONTENT
    }

    /// Not a route: left untouched.
    #[allow(dead_code)]
    fn helper(&self) -> usize {
        self.store.notes.lock().unwrap().len()
    }
}

#[controller]
#[injectable(scope = request)]
pub struct StampController {
    stamp: Arc<RequestStamp>,
}

#[routes]
impl StampController {
    #[get("/stamp")]
    async fn stamp(&self) -> String {
        self.stamp.number.to_string()
    }
}

// --- modules -------------------------------------------------------------------------------

#[module(
    global,
    providers = [value(AppConfig { greeting: "hello" }), value(Counter(AtomicUsize::new(0)))],
    exports = [AppConfig, Counter],
)]
pub struct ConfigModule;

#[module(
    providers = [bind!(dyn Clock => FixedClock)],
    exports = [dyn Clock],
)]
pub struct ClockModule;

#[module(imports = [ClockModule], exports = [module(ClockModule)])]
pub struct CoreModule;

#[module(
    imports = [CoreModule],
    controllers = [NoteController],
    providers = [NoteStore, Greeter, Shouter],
    middlewares = [PoweredBy],
)]
pub struct NotesModule;

#[module(controllers = [StampController], providers = [RequestStamp])]
pub struct StampModule;

#[module(imports = [ConfigModule, NotesModule, StampModule])]
pub struct AppModule;

// --- tests ---------------------------------------------------------------------------------

async fn call(
    router: &axum::Router,
    request: axum::http::Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

fn request(method: &str, uri: &str) -> axum::http::request::Builder {
    axum::http::Request::builder().method(method).uri(uri)
}

#[test]
fn the_macro_built_graph_is_valid() {
    let report = Hornero::new(AppModule).validate().unwrap();
    assert!(report.find("RequestStamp").unwrap().declared);
    assert!(report.find("StampController").unwrap().declared);
    assert!(report.find("NoteController").is_none());
}

#[tokio::test]
async fn macro_built_application_serves_requests() {
    let app = Hornero::new(AppModule).build().await.unwrap();
    let router = app.router();

    // Public route with sync handler, Path + Query extractors, constructor-injected services.
    let (status, headers, body) = call(
        &router,
        request("GET", "/notes/hello/ada")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "hello, ada"));
    assert_eq!(headers["x-powered-by"], "hornero");
    let (_, _, body) = call(
        &router,
        request("GET", "/notes/hello/ada?loud=true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(body, "HELLO, ADA");

    // Controller guard + controller filter.
    let (status, _, body) = call(
        &router,
        request("GET", "/notes/1").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::UNAUTHORIZED, "error: missing api key")
    );

    // Guard data in an extractor, Json body, handler metadata read by an interceptor.
    let (status, headers, body) = call(
        &router,
        request("POST", "/notes")
            .header("x-api-key", "alice")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"text":"remember the milk"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers["x-audited"], "create");
    let note: Note = serde_json::from_str(&body).unwrap();
    assert_eq!(note.text, "remember the milk (by alice)");
    assert_eq!(
        note.created_at, 1_700_000_000,
        "bind!(dyn Clock => FixedClock) via a re-exported module"
    );

    let authed = |method: &str, uri: &str| {
        request(method, uri)
            .header("x-api-key", "alice")
            .body(Body::empty())
            .unwrap()
    };
    let (status, _, _) = call(&router, authed("GET", &format!("/notes/{}", note.id))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&router, authed("DELETE", &format!("/notes/{}", note.id))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = call(&router, authed("GET", &format!("/notes/{}", note.id))).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::NOT_FOUND,
            format!("error: note {} not found", note.id)
        )
    );

    let (status, _, _) = call(
        &router,
        request("TRACE", "/notes").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::RESET_CONTENT);

    // Module middleware is scoped to NotesModule.
    let (_, headers, first) = call(
        &router,
        request("GET", "/stamp").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(headers.get("x-powered-by").is_none());
    let (_, _, second) = call(
        &router,
        request("GET", "/stamp").body(Body::empty()).unwrap(),
    )
    .await;
    assert_ne!(
        first, second,
        "request-scoped controller sees a new stamp per request"
    );

    assert!(app.get::<Counter>().unwrap().0.load(Ordering::SeqCst) >= 6);
}

#[tokio::test]
async fn macro_built_providers_can_be_overridden() {
    struct FrozenClock;
    impl Clock for FrozenClock {
        fn now(&self) -> u64 {
            42
        }
    }
    struct LetEveryoneIn;
    impl Guard for LetEveryoneIn {
        async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
            ctx.parts.extensions.insert(Caller("tester".into()));
            Ok(())
        }
    }
    let clock: Arc<dyn Clock> = Arc::new(FrozenClock);
    let router = Hornero::new(AppModule)
        .override_provider(clock)
        .override_guard::<ApiKeyGuard>(LetEveryoneIn)
        .build()
        .await
        .unwrap()
        .router();
    let (status, _, body) = call(
        &router,
        request("POST", "/notes")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"text":"t"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let note: Note = serde_json::from_str(&body).unwrap();
    assert_eq!((note.created_at, note.text.as_str()), (42, "t (by tester)"));
}

#[test]
fn explicit_scopes_are_pinned() {
    #[injectable(scope = singleton)]
    pub struct Pinned {
        #[allow(dead_code)]
        stamp: Arc<RequestStamp>,
    }
    #[module(providers = [RequestStamp, Pinned])]
    pub struct App;
    let err = Hornero::new(App).validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("Pinned (in App) is pinned to singleton scope"),
        "{err}"
    );
}

#[test]
fn hooks_option_wires_lifecycle() {
    #[derive(Injectable)]
    #[inject(hooks)]
    pub struct WithHooks;
    impl Lifecycle for WithHooks {}
    assert!(<WithHooks as Injectable>::LIFECYCLE.is_some());

    #[derive(Injectable)]
    pub struct WithoutHooks;
    assert!(<WithoutHooks as Injectable>::LIFECYCLE.is_none());
}

#[tokio::test]
async fn provider_option_accepts_any_provider_def() {
    struct Seed(u32);
    struct Derived(u32);

    #[derive(Injectable)]
    struct First {
        derived: Arc<Derived>,
        again: Arc<Derived>,
    }

    #[derive(Injectable)]
    struct Second {
        derived: Arc<Derived>,
    }

    #[module(providers = [
        value(Seed(41)),
        provider(hornero::ProviderDef::factory(|seed: Arc<Seed>| async move {
            Ok::<_, BoxError>(Derived(seed.0 + 1))
        })
        .scope(Scope::Transient)),
        First,
        Second,
    ])]
    struct Root;

    let app = Hornero::new(Root).build().await.unwrap();
    let first = app.get::<First>().unwrap();
    let second = app.get::<Second>().unwrap();
    assert_eq!(first.derived.0, 42);
    assert!(
        Arc::ptr_eq(&first.derived, &first.again),
        "one consumer receives one instance"
    );
    assert!(
        !Arc::ptr_eq(&first.derived, &second.derived),
        "each consumer receives its own instance"
    );
}

// Guards run in order: controller-level ones given as arguments, then the stacked ones top to
// bottom, then the handler's.

#[derive(Clone, Default)]
pub struct Trail(Vec<&'static str>);

macro_rules! trail_guard {
    ($($name:ident => $label:literal),+) => {$(
        #[derive(Injectable)]
        pub struct $name;

        impl Guard for $name {
            async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
                let mut trail = ctx.parts.extensions.remove::<Trail>().unwrap_or_default();
                trail.0.push($label);
                if let Some(Label(label)) = ctx.route.meta::<Label>() {
                    trail.0.push(label);
                }
                ctx.parts.extensions.insert(trail);
                Ok(())
            }
        }
    )+};
}

pub struct Label(&'static str);

trail_guard!(FirstGuard => "first", SecondGuard => "second", ThirdGuard => "third", FourthGuard => "fourth");

#[controller("/trail", guards = [FirstGuard])]
#[guards(SecondGuard, ThirdGuard)]
#[meta(Label("controller"))]
pub struct TrailController;

#[routes]
impl TrailController {
    #[get("/")]
    fn controller_level(&self, Extension(trail): Extension<Trail>) -> String {
        trail.0.join(",")
    }

    #[get("/handler")]
    #[guards(FourthGuard)]
    #[meta(Label("handler"))]
    fn handler_level(&self, Extension(trail): Extension<Trail>) -> String {
        trail.0.join(",")
    }
}

#[module(controllers = [TrailController])]
pub struct TrailModule;

#[tokio::test]
async fn stacked_controller_attributes_follow_the_arguments_in_order() {
    let router = Hornero::new(TrailModule).build().await.unwrap().router();
    let (_, _, body) = call(
        &router,
        request("GET", "/trail").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(body, "first,controller,second,controller,third,controller");

    // The handler's metadata of the same type wins for every guard on that route.
    let (_, _, body) = call(
        &router,
        request("GET", "/trail/handler")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        body,
        "first,handler,second,handler,third,handler,fourth,handler"
    );
}

// `AppGuard<G>` in `providers` is Nest's `{ provide: APP_GUARD, useClass: G }`: it guards every
// route of the application, before the controller's guards.
#[module(imports = [TrailModule], providers = [AppGuard<FourthGuard>])]
pub struct GuardedTrailModule;

#[tokio::test]
async fn app_guard_tokens_in_providers_guard_every_route() {
    let router = Hornero::new(GuardedTrailModule)
        .build()
        .await
        .unwrap()
        .router();
    let (_, _, body) = call(
        &router,
        request("GET", "/trail").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(
        body,
        "fourth,controller,first,controller,second,controller,third,controller"
    );
}

// --- qualifiers and optional dependencies --------------------------------------------------

pub struct Endpoint(&'static str);
pub struct Primary;
pub struct Replica;
/// Never provided: optional dependencies on it receive `None`.
pub struct Tracer;

#[derive(Injectable)]
pub struct ReadWriteSplit {
    #[inject(qualifier = Primary)]
    primary: Arc<Endpoint>,
    #[inject(qualifier = Replica)]
    replica: Arc<Endpoint>,
    #[inject(qualifier = Replica)]
    replica_again: Option<Arc<Endpoint>>,
    tracer: Option<Arc<Tracer>>,
}

pub struct Failover {
    endpoints: Vec<&'static str>,
}

#[injectable]
impl Failover {
    fn new(
        #[inject(qualifier = Primary)] primary: Arc<Endpoint>,
        #[inject(qualifier = Replica)] replica: Option<Arc<Endpoint>>,
        tracer: Option<Arc<Tracer>>,
    ) -> Self {
        assert!(tracer.is_none());
        let endpoints = std::iter::once(primary)
            .chain(replica)
            .map(|e| e.0)
            .collect();
        Self { endpoints }
    }
}

#[derive(Injectable)]
pub struct Archiver;

#[module(
    providers = [
        value(Endpoint("primary")).qualified::<Primary>(),
        factory(|| async { Ok::<_, BoxError>(Endpoint("replica")) }).qualified::<Replica>(),
        Qualified<Archiver, Primary>,
    ],
    exports = [Qualified<Endpoint, Primary>, Qualified<Endpoint, Replica>, Qualified<Archiver, Primary>],
)]
pub struct EndpointModule;

#[module(imports = [EndpointModule], providers = [ReadWriteSplit, Failover])]
pub struct ReplicatedModule;

#[tokio::test]
async fn qualified_and_optional_dependencies_are_injected() {
    let app = Hornero::new(ReplicatedModule).build().await.unwrap();

    let split = app.get::<ReadWriteSplit>().unwrap();
    assert_eq!((split.primary.0, split.replica.0), ("primary", "replica"));
    assert!(Arc::ptr_eq(
        &split.replica,
        split.replica_again.as_ref().unwrap()
    ));
    assert!(split.tracer.is_none());

    assert_eq!(
        app.get::<Failover>().unwrap().endpoints,
        ["primary", "replica"]
    );
    assert!(app.get_qualified::<Archiver, Primary>().is_some());
    assert!(app.get::<Archiver>().is_none());
}

// --- OpenAPI attributes ----------------------------------------------------------------------

#[controller("/documented")]
#[api_tags("Docs")]
#[api_response(401)]
struct DocumentedController;

#[routes]
impl DocumentedController {
    /// Creates something.
    #[post("/")]
    #[api_response(201, description = "Created")]
    #[api_body(String, content_type = "text/plain")]
    async fn create(&self, body: String) -> (StatusCode, String) {
        (StatusCode::CREATED, body)
    }

    #[get("/old")]
    #[deprecated]
    #[api_exclude]
    async fn old(&self) -> &'static str {
        "old"
    }
}

#[module(controllers = [DocumentedController])]
struct DocumentedModule;

/// With or without the `openapi` feature, the `api_*` attributes are accepted and leave the
/// routes alone; calling a `#[deprecated]` handler from the generated registration is not a
/// deprecation warning.
#[tokio::test]
async fn openapi_attributes_do_not_change_routing() {
    let app = Hornero::new(DocumentedModule).build().await.unwrap();
    let router = app.router();
    let res = router
        .clone()
        .oneshot(Request::post("/documented").body(Body::from("hi")).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let res = router
        .oneshot(Request::get("/documented/old").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[test]
fn ui() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/*.rs");
}

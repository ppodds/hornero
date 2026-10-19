use std::any::Any;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::Request;
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use axum::routing::Route;
use tokio::net::{TcpListener, ToSocketAddrs};
use tokio_util::sync::CancellationToken;
use tower::{Layer, Service};
use tower_http::catch_panic::CatchPanicLayer;

use crate::di::graph::{GlobalEnhancers, Graph, ScopeReport};
use crate::di::injectable::{Injectable, Scope};
use crate::di::key::Instance;
use crate::di::key::Key;
use crate::di::module::{LayerFn, Module};
use crate::di::provider::ProviderDef;
use crate::di::runtime::Runtime;
use crate::di::scope::{ErasedHandle, Handle, HandleInner};
use crate::error::{BootError, HttpError, Problem, ShutdownError};
use crate::lifecycle::{ErasedLifecycle, Hook, Shutdown};
use crate::util::BoxError;
use crate::web::enhancer::{self, EnhancerKind, EnhancerRef};
use crate::web::filter::ExceptionFilter;
use crate::web::guard::Guard;
use crate::web::interceptor::Interceptor;
use crate::web::middleware::Middleware;
use crate::web::pipeline;
use crate::web::route::RouteContext;
use crate::web::routes::Routes;

/// The application builder: a root module plus global enhancers, layers and overrides.
///
/// ```ignore
/// #[tokio::main]
/// async fn main() -> Result<(), hornero::BoxError> {
///     Hornero::new(AppModule)
///         .layer(TraceLayer::new_for_http())
///         .listen("0.0.0.0:3000")
///         .await
/// }
///
/// #[test]
/// fn dependency_graph_is_valid() {
///     Hornero::new(AppModule).validate().unwrap();
/// }
/// ```
pub struct Hornero {
    root: Box<dyn Module>,
    prefix: String,
    global: GlobalEnhancers,
    layers: Vec<LayerFn>,
    overrides: Vec<(Key, ProviderDef)>,
    shutdown_timeout: Duration,
}

/// How long [`App::serve`] waits for in-flight requests once shutdown starts.
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`App::serve`] waits, after cancelling the requests that outlived the shutdown
/// timeout, for their connections to close before it runs the shutdown hooks.
const CANCEL_GRACE: Duration = Duration::from_secs(1);

impl Hornero {
    /// Starts configuring an application rooted at `root`.
    pub fn new(root: impl Module) -> Self {
        Self {
            root: Box::new(root),
            prefix: String::new(),
            global: GlobalEnhancers::default(),
            layers: Vec::new(),
            overrides: Vec::new(),
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    /// How long [`App::serve`] (and so [`listen`](Self::listen)) waits for in-flight requests
    /// after shutdown starts, 10 seconds by default. When it elapses, or a second SIGINT/SIGTERM
    /// arrives, the requests still running are cancelled (a handler that has not answered is
    /// dropped and the client gets a 503, a streaming body is cut off) and the shutdown hooks run
    /// anyway. `Duration::MAX` waits forever.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Prefixes every controller route and mount, e.g. `/api`.
    pub fn global_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// A guard applied to every route, resolved in the root module. Runs before the guards modules
    /// declare with [`AppGuard`](crate::AppGuard).
    pub fn guard<G: Guard + Injectable>(mut self) -> Self {
        self.global.guards.push(EnhancerRef::guard::<G>());
        self
    }

    /// An interceptor around every route, resolved in the root module. Global interceptors wrap
    /// controller and handler interceptors, and the ones modules declare with
    /// [`AppInterceptor`](crate::AppInterceptor).
    pub fn interceptor<I: Interceptor + Injectable>(mut self) -> Self {
        self.global
            .interceptors
            .push(EnhancerRef::interceptor::<I>());
        self
    }

    /// An exception filter for every route, resolved in the root module. Tried after the route's
    /// handler- and controller-level filters, in registration order, and before the ones modules
    /// declare with [`AppFilter`](crate::AppFilter).
    pub fn filter<F: ExceptionFilter + Injectable>(mut self) -> Self {
        self.global.filters.push(EnhancerRef::filter::<F>());
        self
    }

    /// A DI-aware middleware around the whole application, including the 404 fallback.
    pub fn middleware<M: Middleware + Injectable>(mut self) -> Self {
        self.global.middlewares.push(EnhancerRef::middleware::<M>());
        self
    }

    /// A tower layer around the whole application (including the 404 fallback).
    /// The first layer added is the outermost.
    pub fn layer<L>(mut self, layer: L) -> Self
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

    /// Replaces every provider of `T` with `value`. The original provider's dependencies are no
    /// longer required.
    pub fn override_provider<T: ?Sized + Send + Sync + 'static>(mut self, value: Arc<T>) -> Self {
        self.overrides
            .push((Key::of::<T>(), ProviderDef::from_arc(value)));
        self
    }

    /// Like [`override_provider`](Self::override_provider), taking the value itself.
    pub fn override_value<T: Send + Sync + 'static>(self, value: T) -> Self {
        self.override_provider(Arc::new(value))
    }

    /// Replaces every provider of `def.key()` with an arbitrary provider definition, e.g. a
    /// qualified provider: `override_with(ProviderDef::value(fake).qualified::<Primary>())`.
    pub fn override_with(mut self, def: ProviderDef) -> Self {
        self.overrides.push((def.key(), def));
        self
    }

    /// Replaces every use of the guard `G` with `guard`, which may be any [`Guard`].
    pub fn override_guard<G: 'static>(mut self, guard: impl Guard) -> Self {
        self.overrides.push(enhancer::guard_override::<G>(guard));
        self
    }

    /// Replaces every use of the interceptor `I` with `interceptor`, which may be any [`Interceptor`].
    pub fn override_interceptor<I: 'static>(mut self, interceptor: impl Interceptor) -> Self {
        self.overrides
            .push(enhancer::interceptor_override::<I>(interceptor));
        self
    }

    /// Replaces every use of the exception filter `F` with `filter`, which may be any
    /// [`ExceptionFilter`].
    pub fn override_filter<F: 'static>(mut self, filter: impl ExceptionFilter) -> Self {
        self.overrides.push(enhancer::filter_override::<F>(filter));
        self
    }

    /// Validates the module graph without constructing anything: visibility, missing and
    /// ambiguous dependencies, cycles, scope rules and route conflicts.
    ///
    /// Route conflicts are found by a dry-run assembly of the router, with placeholder handlers
    /// for the controller routes and the real routers of `mount_router`, so conflicts axum
    /// itself rejects are caught here as well. Routers from factory `mount(..)` need DI and are
    /// only checked by [`build`](Self::build).
    pub fn validate(&self) -> Result<ScopeReport, BootError> {
        let mut graph = self.graph(Shutdown::new())?;
        check_routes(&graph, &self.prefix)?;
        let _dry_run =
            catch_assembly(|| assemble(&mut graph, None, &self.global, Vec::new(), &self.prefix))
                .map_err(|problem| BootError::new(vec![problem]))?;
        Ok(graph.report)
    }

    /// Lists every controller route, like [`validate`](Self::validate) without constructing
    /// anything. Documentation generators read it; see [`Routes`].
    pub fn routes(&self) -> Result<Routes, BootError> {
        let graph = self.graph(Shutdown::new())?;
        check_routes(&graph, &self.prefix)?;
        Ok(Arc::unwrap_or_clone(graph.routes))
    }

    fn graph(&self, shutdown: Shutdown) -> Result<Graph, BootError> {
        Graph::build(
            self.root.as_ref(),
            shutdown,
            &self.prefix,
            &self.global,
            self.overrides.clone(),
        )
    }

    /// Validates, constructs every singleton, assembles the router, then runs `on_module_init`
    /// and `on_application_bootstrap`.
    ///
    /// If anything fails once construction has started, [`Shutdown`] is triggered and the destroy
    /// hooks of every singleton already constructed run (`on_module_destroy`, then
    /// `on_application_shutdown`, in reverse dependency order), so nothing started so far keeps
    /// running. Failures of those hooks are appended to the returned error.
    pub async fn build(self) -> Result<App, BootError> {
        let shutdown = Shutdown::new();
        let mut graph = self.graph(shutdown.clone())?;
        check_routes(&graph, &self.prefix)?;
        let rt = Arc::new(graph.runtime());

        let mut lifecycles = Vec::new();
        for &node in &graph.order {
            if rt.nodes[node].scope != Scope::Singleton {
                continue;
            }
            let built = match rt.instantiate(node, None).await {
                Ok(built) => built,
                Err(err) => {
                    let problem = Problem::Construction {
                        item: err.item.to_string(),
                        source: err.source,
                    };
                    return Err(abort_boot(&shutdown, &lifecycles, vec![problem]).await);
                }
            };
            let _ = rt.singletons[node].set(built.instance);
            if let Some(lifecycle) = built.lifecycle {
                lifecycles.push((rt.nodes[node].label.clone(), lifecycle));
            }
        }

        let singletons = graph.singleton_index();
        // Assembly only needs the constructed singletons, so a route conflict is found before any
        // hook has done I/O. axum reports some conflicts (a mount overlapping a controller route)
        // only by panicking on insertion; surface them as bootstrap errors like everything else.
        let router = catch_assembly(|| {
            assemble(
                &mut graph,
                Some(&rt),
                &self.global,
                self.layers,
                &self.prefix,
            )
        });
        let router = match router {
            Ok(router) => router,
            Err(problem) => {
                return Err(abort_boot(&shutdown, &lifecycles, vec![problem]).await);
            }
        };

        for hook in [Hook::ModuleInit, Hook::ApplicationBootstrap] {
            let problems = run_hooks(lifecycles.iter(), hook).await;
            if !problems.is_empty() {
                return Err(abort_boot(&shutdown, &lifecycles, problems).await);
            }
        }

        if !graph.report.is_empty() {
            tracing::info!("request-scoped components:\n{}", graph.report);
        }
        for (idx, node) in graph.nodes.iter().enumerate() {
            for key in &node.absent {
                tracing::debug!(consumer = %graph.describe(idx), dependency = %key, "optional dependency is not provided");
            }
        }

        Ok(App {
            router,
            rt,
            singletons,
            lifecycles,
            shutdown,
            shutdown_timeout: self.shutdown_timeout,
            report: graph.report,
            routes: graph.routes,
        })
    }

    /// Builds the application and serves it until SIGINT/SIGTERM or [`Shutdown::trigger`].
    ///
    /// If `addr` cannot be bound, the application is closed again, running its shutdown hooks,
    /// before the error is returned.
    pub async fn listen(self, addr: impl ToSocketAddrs) -> Result<(), BoxError> {
        let app = self.build().await?;
        let bound = async {
            let listener = TcpListener::bind(addr).await?;
            let address = listener.local_addr()?;
            Ok::<_, std::io::Error>((listener, address))
        };
        match bound.await {
            Ok((listener, address)) => {
                tracing::info!(%address, "hornero is listening");
                app.serve(listener).await
            }
            Err(err) => {
                // `build` already ran the init hooks: undo them as a failed bootstrap would. A
                // failing hook is logged; the bind error is the one the caller needs.
                let _ = app.close().await;
                Err(err.into())
            }
        }
    }
}

/// A bootstrapped application.
pub struct App {
    router: Router,
    rt: Arc<Runtime>,
    singletons: HashMap<Key, usize>,
    lifecycles: Vec<(Arc<str>, Arc<dyn ErasedLifecycle>)>,
    shutdown: Shutdown,
    shutdown_timeout: Duration,
    report: ScopeReport,
    routes: Arc<Routes>,
}

impl App {
    /// The assembled axum router. Cheap to clone.
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// Every request-scoped component and why it is request-scoped.
    pub fn scope_report(&self) -> &ScopeReport {
        &self.report
    }

    /// Every controller route; see [`Routes`].
    pub fn routes(&self) -> &Routes {
        &self.routes
    }

    /// The application's [`Shutdown`]; [`trigger`](Shutdown::trigger) it to stop [`serve`](Self::serve).
    pub fn shutdown_handle(&self) -> Shutdown {
        self.shutdown.clone()
    }

    /// Looks up a singleton by type: the one visible from the root module, or the only one.
    ///
    /// This is a service locator, meant for tests and bootstrap code only; handlers and
    /// providers should declare their dependencies instead.
    pub fn get<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.singleton(Key::of::<T>())
    }

    /// Like [`get`](Self::get), for the provider of `T` registered under the qualifier `Q`.
    pub fn get_qualified<T: ?Sized + Send + Sync + 'static, Q: 'static>(&self) -> Option<Arc<T>> {
        self.singleton(Key::of::<T>().qualified::<Q>())
    }

    fn singleton<T: ?Sized + Send + Sync + 'static>(&self, key: Key) -> Option<Arc<T>> {
        let node = *self.singletons.get(&key)?;
        self.rt.singletons[node].get()?.downcast::<T>()
    }

    /// Serves on `listener` with graceful shutdown, then runs the shutdown hooks. Handlers can
    /// extract [`ConnectInfo<SocketAddr>`](axum::extract::ConnectInfo) for the peer address.
    ///
    /// Once shutdown starts, in-flight requests get [`Hornero::shutdown_timeout`] to finish; when it
    /// elapses, or another SIGINT/SIGTERM arrives, the requests still running are cancelled (the
    /// client gets a 503, or a cut-off body) and the shutdown hooks run anyway.
    pub async fn serve(self, listener: TcpListener) -> Result<(), BoxError> {
        let shutdown = self.shutdown.clone();
        let signal = {
            let shutdown = shutdown.clone();
            async move {
                tokio::select! {
                    _ = os_signal() => tracing::info!("shutdown signal received"),
                    _ = shutdown.cancelled() => {}
                }
                shutdown.trigger();
            }
        };
        // Dropping the server future would not stop its connections: axum spawns a task for each.
        let cancel = CancellationToken::new();
        let server = axum::serve(
            listener,
            pipeline::cancel_on(self.router.clone(), cancel.clone())
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(signal);
        let mut server = std::pin::pin!(server.into_future());

        let timeout = self.shutdown_timeout;
        let forced = async {
            shutdown.cancelled().await;
            tokio::select! {
                _ = sleep_for(timeout) => tracing::warn!(
                    timeout = ?timeout,
                    "graceful shutdown timed out; dropping the remaining connections"
                ),
                _ = os_signal() => tracing::warn!(
                    "second shutdown signal received; dropping the remaining connections"
                ),
            }
        };
        let served = tokio::select! {
            result = &mut server => result,
            _ = forced => {
                // No request may still be using a provider while its destroy hook runs.
                cancel.cancel();
                match tokio::time::timeout(CANCEL_GRACE, &mut server).await {
                    Ok(result) => result,
                    Err(_) => {
                        tracing::warn!(
                            "some connections did not close after their requests were cancelled"
                        );
                        Ok(())
                    }
                }
            }
        };
        // The hooks run even when the server failed, so nothing started at boot is left running.
        let closed = self.close().await;
        served?;
        closed?;
        Ok(())
    }

    /// Triggers [`Shutdown`] and runs `on_module_destroy` then `on_application_shutdown`, in
    /// reverse dependency order. Every hook runs even if an earlier one fails.
    pub async fn close(self) -> Result<(), ShutdownError> {
        self.shutdown.trigger();
        let mut problems = run_hooks(self.lifecycles.iter().rev(), Hook::ModuleDestroy).await;
        problems.extend(run_hooks(self.lifecycles.iter().rev(), Hook::ApplicationShutdown).await);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(ShutdownError::new(problems))
        }
    }
}

/// Rolls back a failed bootstrap: triggers [`Shutdown`] and runs the destroy hooks of every
/// singleton constructed so far, appending their failures after the original `problems`.
async fn abort_boot(
    shutdown: &Shutdown,
    lifecycles: &[(Arc<str>, Arc<dyn ErasedLifecycle>)],
    mut problems: Vec<Problem>,
) -> BootError {
    shutdown.trigger();
    problems.extend(run_hooks(lifecycles.iter().rev(), Hook::ModuleDestroy).await);
    problems.extend(run_hooks(lifecycles.iter().rev(), Hook::ApplicationShutdown).await);
    BootError::new(problems)
}

async fn run_hooks<'a>(
    lifecycles: impl Iterator<Item = &'a (Arc<str>, Arc<dyn ErasedLifecycle>)>,
    hook: Hook,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (label, lifecycle) in lifecycles {
        if let Err(source) = lifecycle.run(hook).await {
            tracing::error!(provider = %label, %hook, error = %source, "lifecycle hook failed");
            problems.push(Problem::Hook {
                item: label.to_string(),
                hook,
                source,
            });
        }
    }
    problems
}

/// Sleeps for `duration`; a duration too large for the clock (`Duration::MAX`) waits forever
/// instead of overflowing.
async fn sleep_for(duration: Duration) {
    match tokio::time::Instant::now().checked_add(duration) {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

async fn os_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

pub(crate) fn join_paths(parts: &[&str]) -> String {
    let mut out = String::new();
    for part in parts {
        let part = part.trim_matches('/');
        if !part.is_empty() {
            out.push('/');
            out.push_str(part);
        }
    }
    if out.is_empty() { "/".into() } else { out }
}

/// `/todos/{id}` and `/todos/{key}` conflict in axum, so parameters are compared by position only.
fn route_shape(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.starts_with('{') {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn check_routes(graph: &Graph, prefix: &str) -> Result<(), BootError> {
    let mut seen: HashMap<(Method, String), String> = HashMap::new();
    let mut problems = Vec::new();
    for module in &graph.modules {
        for (_, controller) in &module.controllers {
            for route in &controller.routes {
                let path = join_paths(&[prefix, &controller.prefix, &route.path]);
                let handler = format!("{}::{}", controller.name, route.name);
                if !route.method_supported {
                    problems.push(Problem::InvalidRoute {
                        handler: handler.clone(),
                        reason: format!(
                            "uses the HTTP method `{}`, which axum's router cannot match",
                            route.method
                        ),
                    });
                    continue;
                }
                if let Some(segment) = path
                    .split('/')
                    .find(|segment| segment.starts_with(':') || segment.starts_with('*'))
                {
                    problems.push(Problem::InvalidRoute {
                        handler: handler.clone(),
                        reason: format!(
                            "uses the path segment `{segment}`; axum 0.8 expects `{{param}}` and `{{*rest}}`"
                        ),
                    });
                    continue;
                }
                if let Some(first) =
                    seen.insert((route.method.clone(), route_shape(&path)), handler.clone())
                {
                    problems.push(Problem::DuplicateRoute {
                        method: route.method.to_string(),
                        path,
                        first,
                        second: handler,
                    });
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(BootError::new(problems))
    }
}

fn erased_handle(rt: &Arc<Runtime>, node: usize) -> ErasedHandle {
    if rt.nodes[node].scope == Scope::Singleton {
        let instance = rt.singletons[node]
            .get()
            .expect("singletons are constructed before routes are assembled")
            .clone();
        ErasedHandle(HandleInner::Ready(instance))
    } else {
        ErasedHandle(HandleInner::Lazy {
            rt: rt.clone(),
            node,
        })
    }
}

fn slot_handles<T: ?Sized + Send + Sync + 'static>(
    graph: &Graph,
    rt: &Arc<Runtime>,
    refs: impl IntoIterator<Item = (usize, EnhancerRef)>,
) -> Vec<Handle<T>> {
    refs.into_iter()
        .map(|(module, enhancer)| {
            let node = graph.modules[module].slots[&enhancer.slot_key];
            Handle::from_erased(erased_handle(rt, node))
        })
        .collect()
}

/// Runs a router assembly, turning axum's panic on a conflict into a [`Problem`].
fn catch_assembly(assemble: impl FnOnce() -> Router) -> Result<Router, Problem> {
    std::panic::catch_unwind(AssertUnwindSafe(assemble)).map_err(|panic| Problem::InvalidRoute {
        handler: "the router".into(),
        reason: format!("was rejected by axum: {}", panic_message(panic.as_ref())),
    })
}

/// Assembles the route table. With a runtime this is the real router: enhancers, scopes, layers
/// and the fallback. Without one (`Hornero::validate`) it is a dry run that registers the same
/// paths and methods with placeholder handlers and the real `mount_router` routers, which is all
/// axum needs to report a conflict.
fn assemble(
    graph: &mut Graph,
    rt: Option<&Arc<Runtime>>,
    global: &GlobalEnhancers,
    layers: Vec<LayerFn>,
    prefix: &str,
) -> Router {
    let root = graph.root;
    let mut app = Router::new();

    // Global enhancers: the ones registered on `Hornero` (resolved in the root module), then the
    // ones modules declare with `AppGuard<G>` and friends (resolved where they are declared).
    let globals = |refs: &[EnhancerRef], kind: EnhancerKind| -> Vec<(usize, EnhancerRef)> {
        tagged(root, refs)
            .chain(
                graph
                    .app_enhancers
                    .iter()
                    .filter(|(_, e)| e.kind == kind)
                    .cloned(),
            )
            .collect()
    };
    let global_guards = globals(&global.guards, EnhancerKind::Guard);
    let global_interceptors = globals(&global.interceptors, EnhancerKind::Interceptor);
    let global_filters = globals(&global.filters, EnhancerKind::Filter);

    for module in 0..graph.modules.len() {
        let controllers = std::mem::take(&mut graph.modules[module].controllers);
        let mut router = Router::new();

        for (node, controller) in controllers {
            for route in controller.routes {
                let path = join_paths(&[prefix, &controller.prefix, &route.path]);
                let method_router = match rt {
                    Some(rt) => {
                        let mut meta = controller.meta.clone();
                        meta.extend(&route.meta);
                        let ctx = Arc::new(RouteContext {
                            controller: controller.name.clone(),
                            handler: route.name,
                            method: route.method.clone(),
                            path: path.clone(),
                            meta,
                        });

                        let guards = slot_handles::<dyn crate::web::guard::ErasedGuard>(
                            graph,
                            rt,
                            global_guards
                                .iter()
                                .cloned()
                                .chain(tagged(module, &controller.guards))
                                .chain(tagged(module, &route.guards)),
                        );
                        let interceptors =
                            slot_handles::<dyn crate::web::interceptor::ErasedInterceptor>(
                                graph,
                                rt,
                                global_interceptors
                                    .iter()
                                    .cloned()
                                    .chain(tagged(module, &controller.interceptors))
                                    .chain(tagged(module, &route.interceptors)),
                            );
                        // Most specific first; the first filter that catches an error handles it.
                        let filters = slot_handles::<dyn crate::web::filter::ErasedFilter>(
                            graph,
                            rt,
                            tagged(module, &route.filters)
                                .chain(tagged(module, &controller.filters))
                                .chain(global_filters.iter().cloned()),
                        );

                        let controller_handle = erased_handle(rt, node);
                        let handler_needs_context = controller_handle.0.needs_context();
                        let needs_scope = controller_handle.0.needs_scope()
                            || guards.iter().any(Handle::needs_scope)
                            || interceptors.iter().any(Handle::needs_scope)
                            || filters.iter().any(Handle::needs_scope);

                        let mut method_router = (route.build)(controller_handle);
                        for interceptor in interceptors.into_iter().rev() {
                            method_router =
                                pipeline::with_interceptor(method_router, interceptor, ctx.clone());
                        }
                        if !guards.is_empty() {
                            method_router =
                                pipeline::with_guards(method_router, guards, ctx.clone());
                        }
                        if !filters.is_empty() {
                            method_router =
                                pipeline::with_filters(method_router, filters, ctx.clone());
                        }
                        if needs_scope {
                            method_router = pipeline::with_scope(
                                method_router,
                                rt.clone(),
                                handler_needs_context,
                            );
                        }
                        pipeline::with_problem_405(method_router)
                    }
                    // Dry run: the placeholder handle is never used, handlers only run on requests.
                    None => (route.build)(ErasedHandle(HandleInner::Ready(Instance::new(
                        Arc::new(()),
                    )))),
                };
                router = router.route(&path, method_router);
            }
        }

        for (path, node, mounted) in std::mem::take(&mut graph.modules[module].mounts) {
            let mounted = match (rt, mounted) {
                (Some(rt), _) => rt.singletons[node]
                    .get()
                    .and_then(|instance| instance.downcast::<Router>())
                    .expect("mounted routers are singletons"),
                (None, Some(router)) => Arc::new(router),
                // Factory mounts need DI; only `build` can check them.
                (None, None) => continue,
            };
            let full = join_paths(&[prefix, &path]);
            router = if full == "/" {
                router.merge((*mounted).clone())
            } else {
                router.nest(&full, (*mounted).clone())
            };
        }

        if let Some(rt) = rt {
            router = apply_layers(
                router,
                std::mem::take(&mut graph.modules[module].layers),
                slot_handles(
                    graph,
                    rt,
                    tagged(module, &graph.modules[module].middlewares),
                ),
            );
        }
        app = app.merge(router);
    }

    // The dry run stops at the route table: layers and the fallback cannot cause conflicts.
    let Some(rt) = rt else {
        return app;
    };
    // A router mounted at `/` may bring its own fallback (an SPA's `index.html`, say), which
    // `merge` kept. Unmatched requests go to the router as it is now, so they reach that fallback
    // or axum's bare 404, and only the latter becomes problem+json.
    let unmatched = pipeline::not_found_as_problem(app.clone());
    app = app.fallback_service(unmatched);
    // Inside the global layers and middleware, so a panic's 500 still passes through them (CORS,
    // request ids, tracing), but outside routing and module layers so it covers the fallback too.
    app = app.layer(CatchPanicLayer::custom(panic_response));
    app = apply_layers(
        app,
        layers,
        slot_handles(graph, rt, tagged(root, &global.middlewares)),
    );
    // Outermost, so every 5xx is logged exactly once.
    pipeline::log_server_errors(app)
}

fn tagged(module: usize, refs: &[EnhancerRef]) -> impl Iterator<Item = (usize, EnhancerRef)> + '_ {
    refs.iter().map(move |enhancer| (module, enhancer.clone()))
}

/// Tower layers sit inside DI middleware; within each group the first declared is outermost.
fn apply_layers(
    mut router: Router,
    layers: Vec<LayerFn>,
    middlewares: Vec<Handle<dyn crate::web::middleware::ErasedMiddleware>>,
) -> Router {
    for layer in layers.into_iter().rev() {
        router = layer(router);
    }
    for middleware in middlewares.into_iter().rev() {
        router = pipeline::with_middleware(router, middleware);
    }
    router
}

fn panic_message(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("unknown panic")
}

fn panic_response(panic: Box<dyn Any + Send + 'static>) -> Response {
    HttpError::internal("internal server error")
        .with_source(format!(
            "handler panicked: {}",
            panic_message(panic.as_ref())
        ))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{join_paths, panic_message, route_shape};

    #[test]
    fn paths_are_joined_with_single_slashes() {
        assert_eq!(join_paths(&[]), "/");
        assert_eq!(join_paths(&["", "/", ""]), "/");
        assert_eq!(join_paths(&["/api/", "todos", "/{id}/"]), "/api/todos/{id}");
        assert_eq!(join_paths(&["api", "", "/"]), "/api");
        assert_eq!(join_paths(&["", "/todos"]), "/todos");
    }

    #[test]
    fn route_shapes_ignore_parameter_names_only() {
        assert_eq!(route_shape("/todos/{id}"), route_shape("/todos/{todo_id}"));
        // axum rejects a parameter and a catch-all at one position as well.
        assert_eq!(route_shape("/files/{id}"), route_shape("/files/{*rest}"));
        assert_ne!(route_shape("/todos/{id}"), route_shape("/todos/{id}/done"));
        assert_ne!(route_shape("/todos/new"), route_shape("/todos/{id}"));
        assert_eq!(route_shape("/"), "/");
    }

    #[test]
    fn panic_messages_are_read_from_both_payload_types() {
        let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("owned"));
        let borrowed: Box<dyn std::any::Any + Send> = Box::new("borrowed");
        let other: Box<dyn std::any::Any + Send> = Box::new(42);
        assert_eq!(panic_message(owned.as_ref()), "owned");
        assert_eq!(panic_message(borrowed.as_ref()), "borrowed");
        assert_eq!(panic_message(other.as_ref()), "unknown panic");
    }
}

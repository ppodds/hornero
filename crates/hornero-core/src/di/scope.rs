use std::convert::Infallible;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, Method, Uri, Version};
use tokio::sync::OnceCell;

use super::key::Instance;
use super::provider::Builtin;
use super::runtime::{ResolveError, Runtime};
use crate::error::HttpError;

/// A snapshot of the request head, injectable into request-scoped providers as
/// `Arc<RequestContext>`. hornero's counterpart of Nest's `REQUEST` token.
///
/// A component sees the request head as it was when the component was constructed. The snapshot
/// is taken just before a request-scoped guard, interceptor or handler is built, and dropped again
/// after every guard and interceptor, because those may change the request. So data a guard puts
/// into the extensions (an authenticated user, say) is visible to request-scoped components built
/// after it, while components built earlier keep the snapshot they got.
pub struct RequestContext {
    method: Method,
    uri: Uri,
    version: Version,
    headers: HeaderMap,
    extensions: Extensions,
}

impl RequestContext {
    pub(crate) fn capture(head: RequestHead<'_>) -> Self {
        let mut extensions = head.extensions.clone();
        // The scope lives in the extensions and owns this context: drop it to avoid an Arc cycle.
        extensions.remove::<ScopeCell>();
        Self {
            method: head.method.clone(),
            uri: head.uri.clone(),
            version: head.version,
            headers: head.headers.clone(),
            extensions,
        }
    }

    /// The HTTP method.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The request URI.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The HTTP version.
    pub fn version(&self) -> Version {
        self.version
    }

    /// The request headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The request extensions, including anything guards and middleware stored there.
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// Shorthand for `extensions().get::<T>()`.
    pub fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<&T> {
        self.extensions.get::<T>()
    }
}

/// Borrowed view of a request head, from either `Parts` or a full `Request`.
#[derive(Clone, Copy)]
pub(crate) struct RequestHead<'a> {
    method: &'a Method,
    uri: &'a Uri,
    version: Version,
    headers: &'a HeaderMap,
    extensions: &'a Extensions,
}

impl<'a> RequestHead<'a> {
    pub(crate) fn from_parts(parts: &'a Parts) -> Self {
        Self {
            method: &parts.method,
            uri: &parts.uri,
            version: parts.version,
            headers: &parts.headers,
            extensions: &parts.extensions,
        }
    }

    pub(crate) fn from_request(req: &'a Request) -> Self {
        Self {
            method: req.method(),
            uri: req.uri(),
            version: req.version(),
            headers: req.headers(),
            extensions: req.extensions(),
        }
    }
}

/// The request context snapshots of one request.
#[derive(Default)]
struct ContextState {
    /// The snapshot components built now will get; `None` once the request may have changed.
    current: Option<Instance>,
    /// The most recent snapshot, kept for filters that run after the request is gone.
    last: Option<Instance>,
}

/// Per-request cache of request-scoped instances.
pub(crate) struct RequestScope {
    rt: Arc<Runtime>,
    slots: Box<[OnceCell<Instance>]>,
    context: Mutex<ContextState>,
    /// Whether the controller of the current route needs the context (primed by [`ScopeExt`]).
    handler_needs_context: bool,
    /// Whether an exception filter of the route needs the context.
    filters_need_context: AtomicBool,
}

impl RequestScope {
    pub(crate) fn new(rt: Arc<Runtime>, handler_needs_context: bool) -> Self {
        let slots = (0..rt.slot_count).map(|_| OnceCell::new()).collect();
        Self {
            rt,
            slots,
            context: Mutex::new(ContextState::default()),
            handler_needs_context,
            filters_need_context: AtomicBool::new(false),
        }
    }

    /// Called by the filter layer: make sure some snapshot exists when an error reaches the filters.
    pub(crate) fn keep_context_for_filters(&self) {
        self.filters_need_context.store(true, Ordering::Relaxed);
    }

    /// Takes a snapshot of `head` if none is current and `needed` (a component is about to be
    /// built from it), or if filters need one and no snapshot was taken yet. When only filters
    /// need the context, that is one capture in the first stage (for a request an interceptor
    /// short-circuits) and one at the handler if a guard or interceptor ran since.
    pub(crate) fn prime_context(&self, head: RequestHead<'_>, needed: bool) {
        let mut state = self.context.lock().unwrap();
        if state.current.is_some() {
            return;
        }
        if needed || (state.last.is_none() && self.filters_need_context.load(Ordering::Relaxed)) {
            let snapshot = Instance::new(Arc::new(RequestContext::capture(head)));
            state.last = Some(snapshot.clone());
            state.current = Some(snapshot);
        }
    }

    /// Takes a fresh snapshot for filters when a stage rejects the request.
    pub(crate) fn capture_for_filters(&self, head: RequestHead<'_>) {
        if self.filters_need_context.load(Ordering::Relaxed) {
            self.prime_context(head, true);
        }
    }

    /// The request may have changed: the next component that needs the context gets a new snapshot.
    pub(crate) fn invalidate_context(&self) {
        self.context.lock().unwrap().current = None;
    }

    pub(crate) async fn get(&self, idx: usize) -> Result<Instance, ResolveError> {
        let node = &self.rt.nodes[idx];
        if node.builtin == Builtin::RequestContext {
            let state = self.context.lock().unwrap();
            return state
                .current
                .clone()
                .or_else(|| state.last.clone())
                .ok_or_else(|| ResolveError {
                    item: node.label.clone(),
                    source: "the request context was not captured".into(),
                });
        }
        let slot = node.slot.expect("request-scoped nodes have a slot");
        self.slots[slot]
            .get_or_try_init(|| async {
                self.rt
                    .instantiate(idx, Some(self))
                    .await
                    .map(|built| built.instance)
            })
            .await
            .cloned()
    }
}

/// Stored in the request extensions by routes that touch request-scoped components.
#[derive(Clone)]
pub(crate) struct ScopeCell(pub(crate) Arc<RequestScope>);

/// Extractor giving generated handlers access to the request scope. Not meant for direct use.
#[doc(hidden)]
pub struct ScopeExt(Option<Arc<RequestScope>>);

impl<S: Send + Sync> FromRequestParts<S> for ScopeExt {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let scope = parts
            .extensions
            .get::<ScopeCell>()
            .map(|cell| cell.0.clone());
        if let Some(scope) = &scope {
            // The last stage that still has the request: filters built after a failing handler
            // must see what the guards wrote, not the snapshot taken before them.
            let needed =
                scope.handler_needs_context || scope.filters_need_context.load(Ordering::Relaxed);
            scope.prime_context(RequestHead::from_parts(parts), needed);
        }
        Ok(Self(scope))
    }
}

#[derive(Clone)]
pub(crate) enum HandleInner {
    Ready(Instance),
    Lazy { rt: Arc<Runtime>, node: usize },
}

impl HandleInner {
    pub(crate) fn needs_scope(&self) -> bool {
        match self {
            HandleInner::Ready(_) => false,
            HandleInner::Lazy { rt, node } => rt.nodes[*node].slot.is_some(),
        }
    }

    pub(crate) fn needs_context(&self) -> bool {
        match self {
            HandleInner::Ready(_) => false,
            HandleInner::Lazy { rt, node } => rt.nodes[*node].needs_context,
        }
    }
}

/// Untyped [`Handle`], passed from the bootstrap to generated route builders.
#[doc(hidden)]
pub struct ErasedHandle(pub(crate) HandleInner);

/// How a route reaches a component that may be a singleton or request-scoped.
#[doc(hidden)]
pub struct Handle<T: ?Sized> {
    inner: HandleInner,
    _type: PhantomData<fn() -> Arc<T>>,
}

impl<T: ?Sized> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _type: PhantomData,
        }
    }
}

impl<T: ?Sized + Send + Sync + 'static> Handle<T> {
    pub(crate) fn from_erased(handle: ErasedHandle) -> Self {
        Self {
            inner: handle.0,
            _type: PhantomData,
        }
    }

    pub(crate) fn needs_scope(&self) -> bool {
        self.inner.needs_scope()
    }

    pub(crate) fn needs_context(&self) -> bool {
        self.inner.needs_context()
    }

    /// Snapshots the request context now if resolving this handle will need it.
    pub(crate) fn prime(&self, scope: Option<&RequestScope>, head: RequestHead<'_>) {
        if let Some(scope) = scope {
            scope.prime_context(head, self.needs_context());
        }
    }

    pub(crate) async fn resolve(&self, scope: Option<&RequestScope>) -> Result<Arc<T>, HttpError> {
        let instance = match &self.inner {
            HandleInner::Ready(instance) => instance.clone(),
            HandleInner::Lazy { rt, node } => rt.get(*node, scope).await.map_err(|err| {
                HttpError::internal("failed to construct a request-scoped component")
                    .with_source(format!("{}: {}", err.item, err.source))
            })?,
        };
        instance.downcast::<T>().ok_or_else(|| {
            HttpError::internal("internal error").with_source(format!(
                "hornero: instance is not of type `{}`",
                std::any::type_name::<T>()
            ))
        })
    }

    pub async fn resolve_in(&self, scope: &ScopeExt) -> Result<Arc<T>, HttpError> {
        self.resolve(scope.0.as_deref()).await
    }
}

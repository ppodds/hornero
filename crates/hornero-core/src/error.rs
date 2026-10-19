use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};

use crate::lifecycle::Hook;
use crate::util::BoxError;

/// An error that renders as an HTTP response. hornero's replacement for Nest's `HttpException`.
///
/// Any `std::error::Error` converts into a 500 through `?`, with the original error kept as the
/// source (logged, never sent to the client). An [`ExceptionFilter`](crate::ExceptionFilter) can
/// turn such an error back into a specific response by checking the source's type with
/// [`is`](Self::is) or [`downcast_ref`](Self::downcast_ref), Nest's `@Catch(TodoNotFound)`.
/// Errors no filter catches stay 500s.
///
/// The default rendering is RFC 9457 `application/problem+json`. The error is also attached to
/// the response extensions so an exception filter can re-render it, and so hornero's router can log
/// it when the final response is a 5xx.
pub struct HttpError {
    status: StatusCode,
    message: Cow<'static, str>,
    details: Option<Value>,
    source: Option<BoxError>,
    // Boxed: a `HeaderMap` inline would make every `Result<_, HttpError>` large.
    headers: Box<HeaderMap>,
}

impl HttpError {
    /// An error with any status. `message` is sent to the client as the problem's `detail`.
    pub fn new(status: StatusCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status,
            message: message.into(),
            details: None,
            source: None,
            headers: Box::default(),
        }
    }

    /// 400 Bad Request.
    pub fn bad_request(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// 401 Unauthorized, carrying `WWW-Authenticate: Bearer`; replace the challenge with
    /// [`with_header`](Self::with_header).
    pub fn unauthorized(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
            .with_header(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))
    }

    /// 403 Forbidden.
    pub fn forbidden(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    /// 404 Not Found.
    pub fn not_found(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    /// 409 Conflict.
    pub fn conflict(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// 422 Unprocessable Entity.
    pub fn unprocessable(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    /// 500 Internal Server Error.
    pub fn internal(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    /// Wraps an arbitrary error as a 500, keeping it as the (private) source.
    pub fn from_boxed(source: BoxError) -> Self {
        Self::internal("internal server error").with_source(source)
    }

    /// Structured details rendered under `errors` (validation failures, for example).
    pub fn with_details(mut self, details: impl Serialize) -> Self {
        self.details = match serde_json::to_value(details) {
            Ok(value) => Some(value),
            Err(err) => {
                // Details are best-effort, but a silently missing `errors` member is hard to debug.
                tracing::warn!(error = %err, "HttpError details failed to serialize and were dropped");
                None
            }
        };
        self
    }

    /// Sets a response header, replacing any existing value for `name`. Headers travel with the
    /// error, so an [`ExceptionFilter`](crate::ExceptionFilter) that builds its own response can
    /// copy them from [`headers`](Self::headers).
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// The underlying cause: logged with 5xx errors, never sent to the client.
    pub fn with_source(mut self, source: impl Into<BoxError>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The response status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The client-facing message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The structured details, if any.
    pub fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }

    /// The response headers set with [`with_header`](Self::with_header) (plus the default
    /// `WWW-Authenticate` of [`unauthorized`](Self::unauthorized)).
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The underlying cause, if any.
    pub fn source(&self) -> Option<&(dyn std::error::Error + Send + Sync + 'static)> {
        self.source.as_deref()
    }

    /// The underlying cause as an `E`, when it is one: the error `?` converted, or the one passed
    /// to [`from_boxed`](Self::from_boxed) or [`with_source`](Self::with_source).
    pub fn downcast_ref<E: std::error::Error + 'static>(&self) -> Option<&E> {
        self.source.as_deref()?.downcast_ref::<E>()
    }

    /// Whether the underlying cause is an `E`; see [`downcast_ref`](Self::downcast_ref).
    pub fn is<E: std::error::Error + 'static>(&self) -> bool {
        self.downcast_ref::<E>().is_some()
    }

    /// The RFC 9457 problem document used by the default rendering.
    pub fn to_problem(&self) -> Value {
        let mut problem = json!({
            "type": "about:blank",
            "title": self.status.canonical_reason().unwrap_or("Error"),
            "status": self.status.as_u16(),
            "detail": self.message,
        });
        if let Some(details) = &self.details {
            problem["errors"] = details.clone();
        }
        problem
    }

    /// Logs the error with its source chain. hornero's router calls this for responses that are
    /// still 5xx after exception filters ran.
    pub(crate) fn log(&self) {
        match &self.source {
            Some(source) => {
                let mut chain = source.to_string();
                let mut cause = source.source();
                while let Some(err) = cause {
                    chain.push_str(": ");
                    chain.push_str(&err.to_string());
                    cause = err.source();
                }
                tracing::error!(status = %self.status, error = %chain, "{}", self.message);
            }
            None => tracing::error!(status = %self.status, "{}", self.message),
        }
    }
}

impl<E: std::error::Error + Send + Sync + 'static> From<E> for HttpError {
    fn from(err: E) -> Self {
        Self::from_boxed(Box::new(err))
    }
}

impl fmt::Debug for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpError")
            .field("status", &self.status)
            .field("message", &self.message)
            .field("details", &self.details)
            .field("source", &self.source)
            .finish()
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

/// Attached to responses produced from an [`HttpError`], so filters can find the original error.
#[derive(Clone)]
pub(crate) struct ErrorExtension(pub(crate) Arc<HttpError>);

impl HttpError {
    /// Renders the default problem+json body without consuming the error.
    pub fn render(&self) -> Response {
        let mut response = (
            self.status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            self.to_problem().to_string(),
        )
            .into_response();
        response.headers_mut().extend(self.headers.as_ref().clone());
        response
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let mut response = self.render();
        response
            .extensions_mut()
            .insert(ErrorExtension(Arc::new(self)));
        response
    }
}

/// Everything that went wrong while bootstrapping, reported together.
pub struct BootError {
    problems: Vec<Problem>,
}

/// One bootstrap problem. Rendered with a hint where hornero can guess the fix.
#[derive(Debug)]
#[non_exhaustive]
pub enum Problem {
    /// A dependency is not visible from the consumer's module.
    MissingDependency {
        /// The component that declares the dependency, with its module.
        consumer: String,
        /// Short type name of the missing dependency, with its qualifier (`Pool@Primary`) if any.
        dependency: String,
        /// How to make it visible: which module provides it, and what to import or export.
        hint: String,
    },
    /// A dependency is visible from several sources at the same level.
    AmbiguousDependency {
        /// The component that declares the dependency, with its module.
        consumer: String,
        /// Short type name of the dependency.
        dependency: String,
        /// Every provider it could resolve to.
        candidates: Vec<String>,
        /// How to settle it: import one source only, or, for instances of one module type, share
        /// one instance or give each instance's providers a type of its own.
        hint: String,
    },
    /// A module registers the same type, under the same qualifier, twice.
    DuplicateProvider {
        /// Short type name of the provider, with its qualifier if any.
        provider: String,
        /// The module registering it.
        module: String,
    },
    /// Providers depend on each other in a cycle.
    CircularDependency {
        /// The cycle, starting and ending at the same provider.
        path: Vec<String>,
    },
    /// Modules import each other in a cycle.
    CircularImport {
        /// The cycle, starting and ending at the same module.
        path: Vec<String>,
    },
    /// A module exports a type it neither provides nor imports.
    InvalidExport {
        /// The exporting module.
        module: String,
        /// Short type name of the export.
        export: String,
    },
    /// A module re-exports a module it does not import.
    InvalidModuleExport {
        /// The exporting module.
        module: String,
        /// Short type name of the re-exported module.
        export: String,
    },
    /// A module that carries data is imported more than once without declaring its
    /// [`Identity`](crate::Identity), so hornero cannot tell whether the imports are one module.
    UndeclaredModuleIdentity {
        /// The module.
        module: String,
        /// The module performing each import, `None` for the application root.
        imported_by: Vec<Option<String>>,
    },
    /// Several instances of one module type declare the same global enhancer, which would run
    /// once per instance on every route.
    DuplicateGlobalEnhancer {
        /// The registration, such as `AppGuard<AuthGuard>`.
        enhancer: String,
        /// The module type.
        module: String,
        /// The instances declaring it.
        instances: Vec<String>,
    },
    /// A component ends up in a scope it cannot have: middleware, mounts or providers with hooks
    /// that are not singletons, or a pinned provider pulled into request scope.
    ScopeViolation {
        /// The offending component, with its module.
        item: String,
        /// What is wrong, e.g. `is pinned to singleton scope but depends on a request-scoped provider`.
        reason: String,
        /// The dependency chain that made it request-scoped, when bubbling caused the problem.
        chain: Vec<String>,
    },
    /// An override replaces a type that no module provides.
    UnusedOverride {
        /// Short type name of the overridden type.
        provider: String,
    },
    /// A provider registration that is itself malformed, such as `with_hooks::<T>()` on a provider
    /// of another type.
    InvalidProvider {
        /// The provider, with its module.
        provider: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A route axum cannot serve: `:param` path syntax, an unsupported method, or a path axum rejects.
    InvalidRoute {
        /// The handler, as `Controller::method`.
        handler: String,
        /// What is wrong with the route.
        reason: String,
    },
    /// Two handlers serve the same method and path.
    DuplicateRoute {
        /// The HTTP method.
        method: String,
        /// The full path, including prefixes.
        path: String,
        /// The handler registered first.
        first: String,
        /// The handler registered second.
        second: String,
    },
    /// A singleton failed to construct while the application booted.
    Construction {
        /// The provider, with its module.
        item: String,
        /// The error its constructor or factory returned.
        source: BoxError,
    },
    /// A lifecycle hook returned an error.
    Hook {
        /// The provider, with its module.
        item: String,
        /// The hook that failed.
        hook: Hook,
        /// The error the hook returned.
        source: BoxError,
    },
}

impl BootError {
    pub(crate) fn new(problems: Vec<Problem>) -> Self {
        debug_assert!(!problems.is_empty());
        Self { problems }
    }

    /// Every problem found, in the order they were detected.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }
}

impl Problem {
    fn title(&self) -> &'static str {
        match self {
            Problem::MissingDependency { .. } => "missing dependency",
            Problem::AmbiguousDependency { .. } => "ambiguous dependency",
            Problem::DuplicateProvider { .. } => "duplicate provider",
            Problem::CircularDependency { .. } => "circular dependency",
            Problem::CircularImport { .. } => "circular module import",
            Problem::InvalidExport { .. } | Problem::InvalidModuleExport { .. } => "invalid export",
            Problem::UndeclaredModuleIdentity { .. } => "module imported more than once",
            Problem::DuplicateGlobalEnhancer { .. } => "duplicate global enhancer",
            Problem::ScopeViolation { .. } => "scope violation",
            Problem::UnusedOverride { .. } => "unused override",
            Problem::InvalidProvider { .. } => "invalid provider",
            Problem::InvalidRoute { .. } => "invalid route",
            Problem::DuplicateRoute { .. } => "duplicate route",
            Problem::Construction { .. } => "provider construction failed",
            Problem::Hook { .. } => "lifecycle hook failed",
        }
    }

    fn write_body(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const INDENT: &str = "    ";
        match self {
            Problem::MissingDependency {
                consumer,
                dependency,
                hint,
            } => {
                writeln!(
                    f,
                    "{INDENT}{consumer} requires `{dependency}`, but no provider of it is visible there."
                )?;
                writeln!(f, "{INDENT}hint: {hint}")
            }
            Problem::AmbiguousDependency {
                consumer,
                dependency,
                candidates,
                hint,
            } => {
                writeln!(
                    f,
                    "{INDENT}{consumer} requires `{dependency}`, which is visible from several sources:"
                )?;
                for candidate in candidates {
                    writeln!(f, "{INDENT}  - {candidate}")?;
                }
                writeln!(f, "{INDENT}hint: {hint}")
            }
            Problem::DuplicateProvider { provider, module } => {
                writeln!(
                    f,
                    "{INDENT}`{provider}` is registered more than once in {module}."
                )?;
                writeln!(
                    f,
                    "{INDENT}hint: register each instance under its own qualifier (`.qualified::<Q>()`), or use a newtype."
                )
            }
            Problem::CircularDependency { path } => {
                writeln!(f, "{INDENT}{}", path.join(" -> "))?;
                writeln!(
                    f,
                    "{INDENT}hint: hornero has no forwardRef; extract the shared logic into a new provider, or decouple with events or channels."
                )
            }
            Problem::CircularImport { path } => {
                writeln!(f, "{INDENT}{}", path.join(" -> "))?;
                writeln!(
                    f,
                    "{INDENT}hint: move the shared providers into a separate module that both modules import."
                )
            }
            Problem::InvalidExport { module, export } => {
                writeln!(
                    f,
                    "{INDENT}{module} exports `{export}`, but neither provides nor imports it."
                )?;
                writeln!(
                    f,
                    "{INDENT}hint: register it in `providers`, or import the module that exports it."
                )
            }
            Problem::InvalidModuleExport { module, export } => {
                writeln!(
                    f,
                    "{INDENT}{module} re-exports the module `{export}`, but does not import it."
                )?;
                writeln!(f, "{INDENT}hint: import `{export}` in {module}.")
            }
            Problem::UndeclaredModuleIdentity {
                module,
                imported_by,
            } => {
                let imports: Vec<String> = imported_by
                    .iter()
                    .map(|importer| match importer {
                        Some(importer) => format!("by {importer}"),
                        None => "as the application root".into(),
                    })
                    .collect();
                writeln!(
                    f,
                    "{INDENT}{module} carries data and is imported more than once: {}.",
                    imports.join(", ")
                )?;
                writeln!(
                    f,
                    "{INDENT}hint: say how its imports relate in `Module::identity`: `Identity::Instance` makes each import a module of its own (Nest's `forRoot`), `Identity::key(..)` merges imports with equal keys, and `Identity::Type` keeps the first import only."
                )
            }
            Problem::DuplicateGlobalEnhancer {
                enhancer,
                module,
                instances,
            } => {
                writeln!(
                    f,
                    "{INDENT}`{enhancer}` is declared by several instances of {module}, so it would run once per instance on every route:"
                )?;
                for instance in instances {
                    writeln!(f, "{INDENT}  - {instance}")?;
                }
                writeln!(
                    f,
                    "{INDENT}hint: declare it in a module imported once, such as the root module, or merge the instances with `Module::identity`."
                )
            }
            Problem::ScopeViolation {
                item,
                reason,
                chain,
            } => {
                writeln!(f, "{INDENT}{item} {reason}.")?;
                if !chain.is_empty() {
                    writeln!(f, "{INDENT}chain: {}", chain.join(" -> "))?;
                }
                Ok(())
            }
            Problem::UnusedOverride { provider } => {
                writeln!(
                    f,
                    "{INDENT}an override was registered for `{provider}`, but no module provides it."
                )
            }
            Problem::InvalidProvider { provider, reason } => {
                writeln!(f, "{INDENT}{provider} is registered incorrectly: {reason}.")?;
                writeln!(
                    f,
                    "{INDENT}hint: `with_hooks::<T>()` takes the type the provider produces; check the type parameter."
                )
            }
            Problem::InvalidRoute { handler, reason } => writeln!(f, "{INDENT}{handler} {reason}."),
            Problem::DuplicateRoute {
                method,
                path,
                first,
                second,
            } => writeln!(
                f,
                "{INDENT}{method} {path} is handled by both {first} and {second}."
            ),
            Problem::Construction { item, source } => {
                writeln!(f, "{INDENT}{item} failed to construct: {source}")
            }
            Problem::Hook { item, hook, source } => {
                writeln!(f, "{INDENT}{item} failed in {hook}: {source}")
            }
        }
    }
}

/// Writes `"{headline}: N problem(s)"` followed by every problem, numbered.
fn write_problems(f: &mut fmt::Formatter<'_>, headline: &str, problems: &[Problem]) -> fmt::Result {
    let count = problems.len();
    writeln!(
        f,
        "{headline}: {count} problem{}",
        if count == 1 { "" } else { "s" }
    )?;
    for (index, problem) in problems.iter().enumerate() {
        writeln!(f)?;
        writeln!(f, "[{}] {}", index + 1, problem.title())?;
        problem.write_body(f)?;
    }
    Ok(())
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_problems(f, "failed to bootstrap application", &self.problems)
    }
}

impl fmt::Debug for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for BootError {}

/// The application failed to shut down cleanly: one or more `on_module_destroy` or
/// `on_application_shutdown` hooks returned an error. Every hook still ran.
pub struct ShutdownError {
    problems: Vec<Problem>,
}

impl ShutdownError {
    pub(crate) fn new(problems: Vec<Problem>) -> Self {
        debug_assert!(!problems.is_empty());
        Self { problems }
    }

    /// Every hook failure, in the order they happened.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }
}

impl fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_problems(f, "failed to shut down application", &self.problems)
    }
}

impl fmt::Debug for ShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ShutdownError {}

use std::fmt;
use std::future::Future;

use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};

use crate::util::{BoxError, BoxFuture};

/// Async lifecycle hooks, the counterpart of Nest's `OnModuleInit` and friends.
///
/// Rust has no specialization, so hornero cannot discover that a provider implements this trait.
/// Opt in with `#[injectable(hooks)]` (or `const LIFECYCLE = hornero::hooks::<Self>()` by hand).
/// Hooks are only supported on singletons; the bootstrap fails if a provider with hooks
/// ends up request-scoped or transient.
///
/// `Drop` cannot await, so cleanup that needs I/O (closing pools, flushing buffers) belongs in
/// [`on_module_destroy`](Lifecycle::on_module_destroy) or
/// [`on_application_shutdown`](Lifecycle::on_application_shutdown).
pub trait Lifecycle: Send + Sync + 'static {
    /// Called after every singleton has been constructed, in dependency order.
    fn on_module_init(&self) -> impl Future<Output = Result<(), BoxError>> + Send {
        async { Ok(()) }
    }

    /// Called after every `on_module_init` hook has completed, before the server starts.
    fn on_application_bootstrap(&self) -> impl Future<Output = Result<(), BoxError>> + Send {
        async { Ok(()) }
    }

    /// Called during shutdown, after the server stopped accepting requests, in reverse dependency order.
    fn on_module_destroy(&self) -> impl Future<Output = Result<(), BoxError>> + Send {
        async { Ok(()) }
    }

    /// Called after every `on_module_destroy` hook, in reverse dependency order.
    fn on_application_shutdown(&self) -> impl Future<Output = Result<(), BoxError>> + Send {
        async { Ok(()) }
    }
}

/// Names a lifecycle hook, used in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hook {
    /// [`Lifecycle::on_module_init`].
    ModuleInit,
    /// [`Lifecycle::on_application_bootstrap`].
    ApplicationBootstrap,
    /// [`Lifecycle::on_module_destroy`].
    ModuleDestroy,
    /// [`Lifecycle::on_application_shutdown`].
    ApplicationShutdown,
}

impl fmt::Display for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Hook::ModuleInit => "on_module_init",
            Hook::ApplicationBootstrap => "on_application_bootstrap",
            Hook::ModuleDestroy => "on_module_destroy",
            Hook::ApplicationShutdown => "on_application_shutdown",
        })
    }
}

#[doc(hidden)]
pub trait ErasedLifecycle: Send + Sync {
    fn run(&self, hook: Hook) -> BoxFuture<'_, Result<(), BoxError>>;
}

impl<T: Lifecycle> ErasedLifecycle for T {
    fn run(&self, hook: Hook) -> BoxFuture<'_, Result<(), BoxError>> {
        match hook {
            Hook::ModuleInit => Box::pin(self.on_module_init()),
            Hook::ApplicationBootstrap => Box::pin(self.on_application_bootstrap()),
            Hook::ModuleDestroy => Box::pin(self.on_module_destroy()),
            Hook::ApplicationShutdown => Box::pin(self.on_application_shutdown()),
        }
    }
}

/// Built-in singleton signalling application shutdown. Inject `Arc<Shutdown>` into background
/// workers and stop them when [`cancelled`](Shutdown::cancelled) resolves.
#[derive(Clone, Debug, Default)]
pub struct Shutdown {
    token: CancellationToken,
}

impl Shutdown {
    /// A new, untriggered handle. Applications create their own; this is mostly useful in tests.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the shutdown sequence. The server stops accepting requests and runs destroy hooks.
    pub fn trigger(&self) {
        self.token.cancel();
    }

    /// Whether shutdown has been triggered.
    pub fn is_triggered(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves once shutdown has been triggered.
    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    /// A child token that is cancelled together with the application.
    pub fn token(&self) -> CancellationToken {
        self.token.child_token()
    }
}

#[cfg(test)]
mod tests {
    use super::{Hook, Shutdown};

    #[tokio::test]
    async fn clones_and_child_tokens_follow_the_trigger() {
        let shutdown = Shutdown::new();
        let clone = shutdown.clone();
        let token = shutdown.token();
        assert!(!shutdown.is_triggered() && !token.is_cancelled());

        clone.trigger();
        clone.trigger();
        assert!(shutdown.is_triggered());
        assert!(token.is_cancelled());
        // Waiting after the fact resolves at once, and late tokens start out cancelled.
        shutdown.cancelled().await;
        assert!(shutdown.token().is_cancelled());
    }

    #[test]
    fn cancelling_a_child_token_does_not_shut_the_application_down() {
        let shutdown = Shutdown::new();
        shutdown.token().cancel();
        assert!(!shutdown.is_triggered());
    }

    #[test]
    fn hooks_are_named_after_their_methods() {
        let names = [
            Hook::ModuleInit,
            Hook::ApplicationBootstrap,
            Hook::ModuleDestroy,
            Hook::ApplicationShutdown,
        ]
        .map(|hook| hook.to_string());
        assert_eq!(
            names,
            [
                "on_module_init",
                "on_application_bootstrap",
                "on_module_destroy",
                "on_application_shutdown"
            ]
        );
    }
}

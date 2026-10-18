//! A todo API built with every hornero layer. Read it top-down:
//!
//! * [`app`] – the root module: configuration, database, feature modules.
//! * [`auth`] – a module exporting guards and an `AuthUser` "proof" extractor.
//! * [`todo`](mod@todo) – a feature module: entity, repository trait, service, controller,
//!   a domain error mapped by an exception filter, and a request-scoped audit log that pulls
//!   the service into request scope.
//! * [`common`] – global interceptor and exception filter.

pub mod app;
pub mod auth;
pub mod common;
pub mod todo;

use hornero::Hornero;
use tower_http::trace::TraceLayer;

/// The application builder shared by `main` and the end-to-end tests.
pub fn hornero() -> Hornero {
    Hornero::new(app::AppModule)
        .layer(TraceLayer::new_for_http())
        .interceptor::<common::LoggingInterceptor>()
        .filter::<common::ProblemFilter>()
}

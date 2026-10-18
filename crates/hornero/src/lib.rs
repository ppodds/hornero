//! # hornero
//!
//! A NestJS-style web framework for Rust, built on axum and tokio: modules, dependency injection,
//! controllers, guards, interceptors and exception filters, without hiding axum.
//!
//! ```no_run
//! use hornero::prelude::*;
//!
//! #[derive(Injectable)]
//! struct Greeter;
//!
//! impl Greeter {
//!     fn greet(&self, name: &str) -> String {
//!         format!("hello, {name}")
//!     }
//! }
//!
//! #[controller("/hello")]
//! struct HelloController {
//!     greeter: Arc<Greeter>,
//! }
//!
//! #[routes]
//! impl HelloController {
//!     #[get("/{name}")]
//!     async fn hello(&self, Path(name): Path<String>) -> String {
//!         self.greeter.greet(&name)
//!     }
//! }
//!
//! #[module(controllers = [HelloController], providers = [Greeter])]
//! struct AppModule;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), BoxError> {
//!     Hornero::new(AppModule).listen("0.0.0.0:3000").await
//! }
//! ```
//!
//! ## Layers of adoption
//!
//! * **L0** – mount any axum `Router` into a module with `ModuleDef::mount_router`.
//! * **L1** – implement [`Module`], [`Injectable`] and [`Controller`] by hand.
//! * **L2** – use the macros, which expand to exactly the L1 API.
//! * **L3** – add integration crates such as `hornero-config`, `hornero-validation` or `hornero-sea-orm`.
//!
//! ## Features
//!
//! * `openapi` – OpenAPI documents inferred from the route table and handler signatures, as
//!   [`openapi`] (the `hornero-openapi` crate).
//! * `swagger-ui` – `openapi` plus Swagger UI served next to the document.
//!
//! ## Documentation
//!
//! The guide and the API reference live in the repository's `docs/` directory (start with
//! `docs/README.md`), and render next to these API docs as the `hornero-docs` crate.

#![warn(missing_docs)]

pub use hornero_core::*;
pub use hornero_macros::{
    Injectable, api_body, api_exclude, api_response, api_tags, controller, filters, guards,
    injectable, interceptors, meta, module, routes,
};

/// OpenAPI documents and Swagger UI: the `hornero-openapi` crate, enabled by the `openapi` feature.
#[cfg(feature = "openapi")]
pub use hornero_openapi as openapi;

pub use axum;
pub use tower;

/// The types most applications need.
pub mod prelude {
    pub use std::sync::Arc;

    pub use axum::Json;
    pub use axum::extract::{Extension, Path, Query};
    pub use axum::http::StatusCode;
    pub use axum::response::{IntoResponse, Response};

    pub use crate::{
        App, AppFilter, AppGuard, AppInterceptor, BoxError, Controller, ExceptionFilter, Guard,
        GuardContext, Hornero, HttpError, Injectable, Interceptor, Lifecycle, Middleware, Module,
        ModuleDef, Qualified, RequestContext, RouteContext, Scope, Shutdown, api_body, api_exclude,
        api_response, api_tags, bind, controller, filters, guards, injectable, interceptors, meta,
        module, routes,
    };
}

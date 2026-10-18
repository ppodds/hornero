//! # hornero-openapi
//!
//! OpenAPI documents for hornero applications, built on [utoipa](https://docs.rs/utoipa): the
//! counterpart of `@nestjs/swagger`. Usually reached through the `hornero` facade's `openapi`
//! feature as `hornero::openapi`, which also turns on the inference in `#[routes]`.
//!
//! Most of the document is inferred instead of annotated:
//!
//! * **Routes** come from the route table ([`hornero_core::Routes`]), so every controller route of
//!   every module is listed, with its full path.
//! * **Parameters, bodies and responses** come from each handler's signature: `Path<T>`,
//!   `Query<T>`, `Json<T>`, `Form<T>` and `Valid<..>` arguments, and a `Json<T>` (or
//!   `Result<Json<T>, _>`) return type. Body types derive [`ToSchema`]; query types derive
//!   [`IntoParams`] or [`ToSchema`].
//! * **Summary and description** come from the handler's doc comment, `deprecated` from
//!   `#[deprecated]`, and the tag from the controller's name.
//! * **Security** comes from guards implementing [`ApiSecurity`].
//!
//! `#[api_response(...)]`, `#[api_body(...)]`, `#[api_tags(...)]` and `#[api_exclude]` fill in
//! the rest, and [`OpenApiConfig::modify`] edits the finished document.
//!
//! ```rust,ignore
//! use hornero::openapi::{OpenApiConfig, OpenApiModule, ToSchema};
//!
//! #[derive(Serialize, ToSchema)]
//! struct Todo { id: u64, title: String }
//!
//! #[routes]
//! impl TodoController {
//!     /// Finds one todo.
//!     #[get("/{id}")]
//!     #[api_response(404, description = "No todo has this id")]
//!     async fn find_one(&self, Path(id): Path<u64>) -> Result<Json<Todo>, HttpError> { ... }
//! }
//!
//! #[module(imports = [OpenApiModule::for_root(OpenApiConfig::new("Todo API", "1.0")), TodoModule])]
//! struct AppModule;
//! // GET /docs is Swagger UI, GET /docs/openapi.json the document.
//! ```
//!
//! See the guide chapter `docs/guide/16-openapi.md` and the API reference
//! `docs/api/hornero-openapi.md`.

#![warn(missing_docs)]

mod config;
mod document;
mod module;
mod security;
#[cfg(feature = "swagger-ui")]
mod ui;

#[doc(hidden)]
pub mod __private;

pub use config::OpenApiConfig;
pub use document::{document, warnings};
pub use module::OpenApiModule;
pub use security::{ApiSecurity, SecurityScheme, describe_security};

pub use utoipa;
pub use utoipa::openapi::OpenApi;
pub use utoipa::{IntoParams, ToSchema};

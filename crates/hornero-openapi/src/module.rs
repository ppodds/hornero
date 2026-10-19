use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::http::header::CONTENT_TYPE;
use axum::routing::get;
use hornero_core::{BoxError, Module, ModuleDef, Routes};

use crate::config::OpenApiConfig;
use crate::document::document;

/// Serves the OpenAPI document of the application's routes, and Swagger UI with the
/// `swagger-ui` feature: Nest's `SwaggerModule.setup`, as a module to import into the root one.
///
/// ```rust,ignore
/// #[module(imports = [
///     OpenApiModule::for_root(OpenApiConfig::new("Todo API", "1.0")),
///     TodoModule,
/// ])]
/// struct AppModule;
/// ```
///
/// With the default path, `GET /docs` is Swagger UI and `GET /docs/openapi.json` the document,
/// both under `Hornero::global_prefix`. The document is built once at startup from the route table,
/// so it lists every controller route of the application, whichever module it is in.
pub struct OpenApiModule {
    config: OpenApiConfig,
}

impl OpenApiModule {
    /// The module serving the document `config` describes.
    ///
    /// Every call makes a distinct module, so two of them with different paths and
    /// [`filter`](OpenApiConfig::filter)s can serve two documents.
    pub fn for_root(config: OpenApiConfig) -> Self {
        Self { config }
    }
}

impl Module for OpenApiModule {
    fn define(&self, m: &mut ModuleDef) {
        let config = self.config.clone();
        m.mount("/", move |routes: Arc<Routes>| {
            let router = router(&routes, &config);
            async move { router }
        });
    }
}

fn router(routes: &Routes, config: &OpenApiConfig) -> Result<Router, BoxError> {
    if config.path.is_empty() {
        return Err("OpenApiConfig::path cannot be `/`: the documentation needs a path of its own, such as `/docs`".into());
    }
    let json = Bytes::from(document(routes, config).to_json()?);
    let json_path = format!("{}/openapi.json", config.path);
    let router = Router::new().route(
        &json_path,
        get(move || {
            let json = json.clone();
            async move { ([(CONTENT_TYPE, "application/json")], json) }
        }),
    );
    #[cfg(feature = "swagger-ui")]
    let router = crate::ui::routes(router, &config.path, routes.prefix());
    Ok(router)
}

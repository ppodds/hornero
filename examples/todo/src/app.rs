use hornero::openapi::{OpenApiConfig, OpenApiModule};
use hornero::prelude::*;
use hornero_config::{ConfigModule, Source};
use hornero_sea_orm::SeaOrmModule;
use hornero_sea_orm::sea_orm::ConnectOptions;
use serde::Deserialize;

use crate::auth::{AuthConfig, AuthModule};
use crate::todo::TodoModule;

#[derive(Deserialize)]
pub struct ServerConfig {
    pub address: String,
}

#[derive(Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
}

#[module(imports = [
    ConfigModule::for_root(Source::new().toml_required("config.toml").env("TODO_"))
        .section::<ServerConfig>("server")
        .section::<DatabaseConfig>("database")
        .section::<AuthConfig>("auth"),
    SeaOrmModule::for_root_with(|config: Arc<DatabaseConfig>| async move {
        let mut options = ConnectOptions::new(config.url.clone());
        options.max_connections(config.max_connections).sqlx_logging(false);
        Ok::<_, BoxError>(options)
    }),
    // Swagger UI at `/docs`, the document at `/docs/openapi.json`.
    OpenApiModule::for_root(
        OpenApiConfig::new("Todo API", env!("CARGO_PKG_VERSION"))
            .description("The hornero example application. Use `user-token` or `admin-token` to authorize.")
    ),
    AuthModule,
    TodoModule,
])]
pub struct AppModule;

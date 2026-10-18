//! Swagger UI, from the assets `utoipa-swagger-ui` embeds.

use std::sync::Arc;

use axum::Router;
use axum::extract::Path;
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use utoipa_swagger_ui::Config;

/// `{path}` redirects to `{path}/`, which serves the UI so its relative asset URLs resolve under
/// `{path}/`. `prefix` is the global prefix the router is nested under.
pub(crate) fn routes(router: Router, path: &str, prefix: &str) -> Router {
    let config = Arc::new(Config::from(format!("{prefix}{path}/openapi.json")));
    let index = format!("{prefix}{path}/");
    let index_config = config.clone();
    router
        .route(path, get(move || std::future::ready(Redirect::to(&index))))
        .route(
            &format!("{path}/"),
            get(move || std::future::ready(serve("", index_config.clone()))),
        )
        .route(
            &format!("{path}/{{*file}}"),
            get(move |Path(file): Path<String>| std::future::ready(serve(&file, config.clone()))),
        )
}

fn serve(file: &str, config: Arc<Config<'static>>) -> Response {
    match utoipa_swagger_ui::serve(file, config) {
        Ok(Some(file)) => {
            let mut response =
                ([(CONTENT_TYPE, file.content_type)], file.bytes.into_owned()).into_response();
            if file.gzpipped {
                response
                    .headers_mut()
                    .insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            }
            response
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

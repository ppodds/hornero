//! `OpenApiModule` serving the document and Swagger UI.

use axum::http::StatusCode;
use hornero::openapi::{OpenApiConfig, OpenApiModule};
use hornero::prelude::*;
use hornero_testing::TestApp;
use serde_json::Value;

#[controller("/todos")]
struct TodoController;

#[routes]
impl TodoController {
    /// Lists todos.
    #[get("/")]
    async fn list(&self) -> Json<Vec<String>> {
        Json(Vec::new())
    }
}

#[module(controllers = [TodoController])]
struct TodoModule;

#[module(imports = [OpenApiModule::for_root(OpenApiConfig::new("Todo API", "1.0")), TodoModule])]
struct AppModule;

#[tokio::test]
async fn the_document_is_served_under_the_global_prefix() {
    let app = TestApp::new(AppModule)
        .configure(|hornero| hornero.global_prefix("/api"))
        .build()
        .await
        .unwrap();
    let res = app.client().get("/api/docs/openapi.json").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.header("content-type"), Some("application/json"));
    let doc: Value = res.json();
    assert_eq!(doc["info"]["title"], "Todo API");
    // The document lists the application's routes, not its own.
    let paths: Vec<&String> = doc["paths"].as_object().unwrap().keys().collect();
    assert_eq!(paths, ["/api/todos"]);
}

#[tokio::test]
async fn swagger_ui_is_served_next_to_the_document() {
    let app = TestApp::new(AppModule)
        .configure(|hornero| hornero.global_prefix("/api"))
        .build()
        .await
        .unwrap();
    let client = app.client();

    let res = client.get("/api/docs").await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.header("location"), Some("/api/docs/"));

    let res = client.get("/api/docs/").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.header("content-type"), Some("text/html"));
    assert!(res.text().contains("swagger-ui"), "{}", res.text());

    // The initializer points the UI at the document, global prefix included.
    let res = client.get("/api/docs/swagger-initializer.js").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.text().contains("/api/docs/openapi.json"),
        "{}",
        res.text()
    );

    assert_eq!(
        client.get("/api/docs/no-such-file.js").await.status(),
        StatusCode::NOT_FOUND
    );
}

#[module(imports = [
    OpenApiModule::for_root(OpenApiConfig::new("Public API", "1.0").path("/docs/public/")),
    OpenApiModule::for_root(
        OpenApiConfig::new("Nothing", "1.0").path("/docs/none").filter(|_| false)
    ),
    TodoModule,
])]
struct TwoDocsModule;

#[tokio::test]
async fn several_documents_can_be_served_at_different_paths() {
    let app = TestApp::new(TwoDocsModule).build().await.unwrap();
    let client = app.client();
    let public: Value = client.get("/docs/public/openapi.json").await.json();
    assert_eq!(public["info"]["title"], "Public API");
    assert!(public["paths"]["/todos"].is_object());
    let none: Value = client.get("/docs/none/openapi.json").await.json();
    assert_eq!(none["paths"], serde_json::json!({}));
}

#[module(imports = [OpenApiModule::for_root(OpenApiConfig::new("Root", "1.0").path("/")), TodoModule])]
struct RootDocsModule;

#[tokio::test]
async fn the_documentation_needs_a_path_of_its_own() {
    let err = TestApp::new(RootDocsModule).build().await.err().unwrap();
    assert!(
        err.to_string()
            .contains("OpenApiConfig::path cannot be `/`"),
        "{err}"
    );
}

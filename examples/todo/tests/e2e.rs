use std::sync::Mutex;

use hornero::BoxFuture;
use hornero::axum::http::StatusCode;
use hornero_testing::{TestApp, TestClient};
use sea_orm::DbErr;
use serde_json::{Value, json};
use todo::app::DatabaseConfig;
use todo::todo::audit::{AuditEntry, AuditTrail};
use todo::todo::entity::Todo;
use todo::todo::repo::{TodoPatch, TodoRepo};

/// Every test gets its own in-memory database instead of the file configured in `config.toml`.
fn in_memory() -> DatabaseConfig {
    DatabaseConfig {
        url: "sqlite::memory:".into(),
        max_connections: 1,
    }
}

async fn start() -> (TestApp, TestClient, TestClient) {
    let app = TestApp::from_hornero(todo::hornero())
        .override_value(in_memory())
        .build()
        .await
        .unwrap();
    let admin = app.client().with_bearer("admin-token");
    let user = app.client().with_bearer("user-token");
    (app, admin, user)
}

#[test]
fn dependency_graph_is_valid() {
    let report = todo::hornero().validate().unwrap();
    assert_eq!(
        report.find("TodoController").unwrap().chain,
        ["TodoController", "TodoService", "AuditLog [request]"]
    );
    assert!(
        report.find("SeaOrmTodoRepo").is_none(),
        "the repository stays a singleton"
    );
}

#[tokio::test]
async fn crud_round_trip() {
    let (app, admin, user) = start().await;

    let created: Todo = user
        .post("/todos")
        .json(&json!({ "title": "write the design doc" }))
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    assert_eq!(created.title, "write the design doc");
    assert_eq!(created.created_by, "bob", "AuthUser extractor");
    assert!(!created.done);

    let updated: Todo = user
        .patch(&format!("/todos/{}", created.id))
        .json(&json!({ "done": true }))
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert!(updated.done);

    let all: Vec<Todo> = user
        .get("/todos")
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(all, std::slice::from_ref(&updated));

    let forbidden: Value = user
        .delete(&format!("/todos/{}", created.id))
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .json();
    assert_eq!(forbidden["detail"], "requires one of the roles: admin");

    admin
        .delete(&format!("/todos/{}", created.id))
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let missing: Value = admin
        .get(&format!("/todos/{}", created.id))
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .json();
    assert_eq!(missing["detail"], format!("todo {} not found", created.id));
    assert_eq!(
        missing["instance"], "/todos/{id}",
        "ProblemFilter adds the route"
    );
    assert_eq!(missing["handler"], "TodoController::find_one");

    // AuditLog is request-scoped and reads the user from the RequestContext captured after
    // AuthGuard ran.
    let trail = app.get::<AuditTrail>().entries();
    let entry = |user: &str, action: &str| AuditEntry {
        user: user.into(),
        action: action.into(),
        todo: created.id,
    };
    assert_eq!(
        trail,
        [
            entry("bob", "create"),
            entry("bob", "update"),
            entry("alice", "delete")
        ]
    );

    app.close().await.unwrap();
}

#[tokio::test]
async fn requests_without_a_valid_token_are_rejected() {
    let (app, _, _) = start().await;
    let anonymous = app.client();

    let response = anonymous
        .get("/todos")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    // The global ProblemFilter rebuilds the response; the challenge must survive it.
    assert_eq!(response.header("www-authenticate"), Some("Bearer"));
    let problem: Value = response.json();
    assert_eq!(problem["detail"], "missing bearer token");

    anonymous
        .get("/todos")
        .bearer("forged")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invalid_input_is_rejected_with_field_errors() {
    let (_, _, user) = start().await;
    let problem: Value = user
        .post("/todos")
        .json(&json!({ "title": "" }))
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .json();
    assert_eq!(problem["errors"][0]["path"], "title");
    assert_eq!(
        problem["instance"], "/todos",
        "validation errors reach the global filter"
    );

    user.patch("/todos/1")
        .json(&json!({ "title": "" }))
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}

/// Swapping the repository needs no database at all for the code under test.
#[derive(Default)]
struct InMemoryRepo(Mutex<Vec<Todo>>);

impl TodoRepo for InMemoryRepo {
    fn all(&self) -> BoxFuture<'_, Result<Vec<Todo>, DbErr>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().clone()) })
    }
    fn find(&self, id: i32) -> BoxFuture<'_, Result<Option<Todo>, DbErr>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().iter().find(|t| t.id == id).cloned()) })
    }
    fn insert(&self, title: String, created_by: String) -> BoxFuture<'_, Result<Todo, DbErr>> {
        Box::pin(async move {
            let mut todos = self.0.lock().unwrap();
            let todo = Todo {
                id: todos.len() as i32 + 100,
                title,
                done: false,
                created_by,
            };
            todos.push(todo.clone());
            Ok(todo)
        })
    }
    fn update(&self, _: i32, _: TodoPatch) -> BoxFuture<'_, Result<Option<Todo>, DbErr>> {
        Box::pin(async { Ok(None) })
    }
    fn delete(&self, _: i32) -> BoxFuture<'_, Result<bool, DbErr>> {
        Box::pin(async { Err(DbErr::Custom("disk on fire".into())) })
    }
}

#[tokio::test]
async fn the_repository_can_be_replaced() {
    let repo: std::sync::Arc<dyn TodoRepo> = std::sync::Arc::new(InMemoryRepo::default());
    let app = TestApp::from_hornero(todo::hornero())
        .override_value(in_memory())
        .override_provider(repo)
        .build()
        .await
        .unwrap();
    let admin = app.client().with_bearer("admin-token");

    let created: Todo = admin
        .post("/todos")
        .json(&json!({ "title": "in memory" }))
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    assert_eq!(created.id, 100);

    // Infrastructure errors become opaque 500s: the DbErr message is logged, never returned.
    let problem: Value = admin
        .delete("/todos/100")
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert_eq!(problem["detail"], "internal server error");
    assert!(!problem.to_string().contains("disk on fire"));
}

#[test]
fn every_route_is_fully_documented() {
    let routes = todo::hornero().routes().unwrap();
    let config = hornero::openapi::OpenApiConfig::new("Todo API", "1.0");
    assert_eq!(
        hornero::openapi::warnings(&routes, &config),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn the_api_documents_itself() {
    let (_app, _admin, user) = start().await;
    // The document is public: `AuthGuard` guards the controller, not the docs.
    let doc: Value = user
        .get("/docs/openapi.json")
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(doc["info"]["title"], "Todo API");

    let create = &doc["paths"]["/todos"]["post"];
    assert_eq!(create["summary"], "Creates a todo owned by the caller.");
    assert_eq!(
        create["requestBody"]["content"]["application/json"]["schema"],
        json!({ "$ref": "#/components/schemas/CreateTodo" })
    );
    assert_eq!(
        create["responses"]["201"]["content"]["application/json"]["schema"],
        json!({ "$ref": "#/components/schemas/Todo" })
    );
    assert!(
        create["responses"]["401"].is_object(),
        "from the controller's #[api_response]"
    );
    assert!(create["responses"]["422"].is_object(), "from Valid<..>");
    assert_eq!(create["security"], json!([{ "bearer": [] }]));

    let remove = &doc["paths"]["/todos/{id}"]["delete"];
    assert_eq!(remove["parameters"][0]["name"], "id");
    assert_eq!(
        remove["responses"]["204"],
        json!({ "description": "No Content" })
    );
    let todo = &doc["components"]["schemas"]["Todo"];
    assert_eq!(
        todo["properties"]["created_by"]["description"],
        "The user who created it."
    );
    assert_eq!(
        doc["components"]["securitySchemes"]["bearer"]["scheme"],
        "bearer"
    );

    let ui = user.get("/docs/").await.assert_status(StatusCode::OK);
    assert!(ui.text().contains("swagger-ui"));
}

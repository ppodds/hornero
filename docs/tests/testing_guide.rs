//! The examples of `docs/guide/09-testing.md`, run as real tests.
//!
//! The regions between `// ANCHOR:` markers are copied into the guide; `tests/markdown.rs`
//! checks that the copies stay identical.

// ANCHOR: app
use hornero::BoxFuture;
use hornero::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub id: u64,
    pub title: String,
}

#[derive(Serialize, Deserialize)]
pub struct NewTodo {
    pub title: String,
}

pub trait TodoRepo: Send + Sync {
    fn all(&self) -> BoxFuture<'_, Vec<Todo>>;
    fn insert(&self, title: String) -> BoxFuture<'_, Todo>;
}

/// The production repository; a real one would talk to a database.
#[derive(Injectable)]
pub struct SqlTodoRepo;

impl TodoRepo for SqlTodoRepo {
    fn all(&self) -> BoxFuture<'_, Vec<Todo>> {
        Box::pin(async { unimplemented!("SELECT * FROM todos") })
    }

    fn insert(&self, _title: String) -> BoxFuture<'_, Todo> {
        Box::pin(async { unimplemented!("INSERT INTO todos ...") })
    }
}

#[derive(Injectable)]
pub struct TodoService {
    repo: Arc<dyn TodoRepo>,
}

impl TodoService {
    pub async fn list(&self) -> Vec<Todo> {
        self.repo.all().await
    }

    pub async fn create(&self, title: String) -> Result<Todo, HttpError> {
        if title.trim().is_empty() {
            return Err(HttpError::unprocessable("title must not be empty"));
        }
        Ok(self.repo.insert(title).await)
    }
}

/// Requires a bearer token; a real one would verify a JWT.
#[derive(Injectable)]
pub struct AuthGuard;

impl Guard for AuthGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        ctx.parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.starts_with("Bearer "))
            .map(|_| ())
            .ok_or_else(|| HttpError::unauthorized("missing bearer token"))
    }
}

#[controller("/todos")]
#[guards(AuthGuard)]
pub struct TodoController {
    todos: Arc<TodoService>,
}

#[routes]
impl TodoController {
    #[get("/")]
    async fn list(&self) -> Json<Vec<Todo>> {
        Json(self.todos.list().await)
    }

    #[post("/")]
    async fn create(
        &self,
        Json(input): Json<NewTodo>,
    ) -> Result<(StatusCode, Json<Todo>), HttpError> {
        let todo = self.todos.create(input.title).await?;
        Ok((StatusCode::CREATED, Json(todo)))
    }
}

#[module(
    controllers = [TodoController],
    providers = [TodoService, AuthGuard, bind!(dyn TodoRepo => SqlTodoRepo)],
)]
pub struct TodoModule;

#[module(imports = [TodoModule])]
pub struct AppModule;
// ANCHOR_END: app

// ANCHOR: fakes
use std::sync::Mutex;

use hornero_testing::TestApp;

/// An in-memory repository for tests.
#[derive(Default)]
struct InMemoryRepo {
    todos: Mutex<Vec<Todo>>,
}

impl InMemoryRepo {
    fn with(titles: &[&str]) -> Self {
        let repo = Self::default();
        for title in titles {
            repo.push(title.to_string());
        }
        repo
    }

    fn push(&self, title: String) -> Todo {
        let mut todos = self.todos.lock().unwrap();
        let todo = Todo {
            id: todos.len() as u64 + 1,
            title,
        };
        todos.push(todo.clone());
        todo
    }
}

impl TodoRepo for InMemoryRepo {
    fn all(&self) -> BoxFuture<'_, Vec<Todo>> {
        Box::pin(async { self.todos.lock().unwrap().clone() })
    }

    fn insert(&self, title: String) -> BoxFuture<'_, Todo> {
        Box::pin(async move { self.push(title) })
    }
}

/// Lets every request through.
struct AllowAll;

impl Guard for AllowAll {
    async fn can_activate(&self, _ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}
// ANCHOR_END: fakes

// ANCHOR: validate
#[test]
fn the_dependency_graph_is_valid() {
    let report = Hornero::new(AppModule).validate().unwrap();
    assert!(
        report.is_empty(),
        "nothing should be request-scoped:\n{report}"
    );
}
// ANCHOR_END: validate

// ANCHOR: override
#[tokio::test]
async fn lists_todos() {
    let app = TestApp::new(AppModule)
        .override_provider::<dyn TodoRepo>(Arc::new(InMemoryRepo::with(&["write docs"])))
        .override_guard::<AuthGuard>(AllowAll)
        .build()
        .await
        .unwrap();

    let todos: Vec<Todo> = app
        .client()
        .get("/todos")
        .await
        .assert_status(StatusCode::OK)
        .json();

    assert_eq!(
        todos,
        [Todo {
            id: 1,
            title: "write docs".into()
        }]
    );
}
// ANCHOR_END: override

// ANCHOR: real-guard
#[tokio::test]
async fn requires_a_bearer_token() {
    let app = TestApp::new(AppModule)
        .override_provider::<dyn TodoRepo>(Arc::new(InMemoryRepo::default()))
        .build()
        .await
        .unwrap();

    app.client()
        .get("/todos")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    app.client()
        .get("/todos")
        .bearer("token")
        .await
        .assert_status(StatusCode::OK);

    // A client that sends the header with every request.
    let client = app.client().with_bearer("token");
    client
        .post("/todos")
        .json(&NewTodo {
            title: "ship it".into(),
        })
        .await
        .assert_status(StatusCode::CREATED);
}
// ANCHOR_END: real-guard

// ANCHOR: singletons
#[tokio::test]
async fn created_todos_reach_the_repository() {
    let app = TestApp::new(AppModule)
        .override_provider::<dyn TodoRepo>(Arc::new(InMemoryRepo::default()))
        .override_guard::<AuthGuard>(AllowAll)
        .build()
        .await
        .unwrap();

    let created: Todo = app
        .client()
        .post("/todos")
        .json(&NewTodo {
            title: "ship it".into(),
        })
        .await
        .assert_status(StatusCode::CREATED)
        .json();

    // Any singleton can be fetched by type to check side effects.
    let repo = app.get::<dyn TodoRepo>();
    assert_eq!(repo.all().await, [created]);

    app.close().await.unwrap(); // runs the shutdown hooks
}
// ANCHOR_END: singletons

// ANCHOR: errors
#[tokio::test]
async fn blank_titles_are_rejected() {
    let app = TestApp::new(AppModule)
        .override_provider::<dyn TodoRepo>(Arc::new(InMemoryRepo::default()))
        .override_guard::<AuthGuard>(AllowAll)
        .build()
        .await
        .unwrap();

    let res = app
        .client()
        .post("/todos")
        .json(&NewTodo { title: " ".into() })
        .await;

    assert_eq!(res.header("content-type"), Some("application/problem+json"));
    let problem: serde_json::Value = res.assert_status(StatusCode::UNPROCESSABLE_ENTITY).json();
    assert_eq!(problem["detail"], "title must not be empty");
}
// ANCHOR_END: errors

// ANCHOR: unit
#[tokio::test]
async fn service_rejects_blank_titles() {
    // Providers are plain structs: a unit test can build one without the container.
    let service = TodoService {
        repo: Arc::new(InMemoryRepo::default()),
    };

    let err = service.create("   ".into()).await.unwrap_err();

    assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
// ANCHOR_END: unit

// ANCHOR: from-hornero
/// What `main` uses: the root module plus global settings.
fn hornero() -> Hornero {
    Hornero::new(AppModule).global_prefix("/api")
}

#[tokio::test]
async fn tests_can_reuse_the_production_builder() {
    let app = TestApp::from_hornero(hornero())
        .override_provider::<dyn TodoRepo>(Arc::new(InMemoryRepo::default()))
        .override_guard::<AuthGuard>(AllowAll)
        .build()
        .await
        .unwrap();

    app.client()
        .get("/api/todos")
        .await
        .assert_status(StatusCode::OK);
}
// ANCHOR_END: from-hornero

// ANCHOR: unused-override
#[tokio::test]
async fn overrides_must_replace_something() {
    struct NotProvidedAnywhere;

    let err = TestApp::new(AppModule)
        .override_value(NotProvidedAnywhere)
        .build()
        .await
        .err()
        .unwrap();

    assert!(err.to_string().contains("unused override"));
}
// ANCHOR_END: unused-override

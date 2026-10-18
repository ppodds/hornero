# 9. 測試

hornero 的測試分三層，由快到慢：

| 層級 | 工具 | 檢查什麼 |
|---|---|---|
| 依賴圖 | `Hornero::validate()` | 可見性、循環、scope、路由（包括 dry-run 組裝 router 找出 axum 會拒絕的衝突），不建構任何東西 |
| 單元測試 | 直接建構 struct | 單一 provider 的邏輯 |
| 整合測試 | `hornero-testing` 的 `TestApp` | 完整的 HTTP 流程，包括 guard、interceptor、filter、middleware |

```toml
[dev-dependencies]
hornero-testing = { path = "../hornero/crates/hornero-testing" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
serde_json = "1"
```

本章的範例都取自 [`docs/tests/testing_guide.rs`](../tests/testing_guide.rs)，會被當成真正的測試執行。被測試的應用如下：一個 controller、一個依賴 `dyn TodoRepo` 的 service，以及一個要求 bearer token 的 guard。

<!-- snippet: tests/testing_guide.rs#app -->
```rust,ignore
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
```

## 驗證依賴圖

<!-- snippet: tests/testing_guide.rs#validate -->
```rust,ignore
#[test]
fn the_dependency_graph_is_valid() {
    let report = Hornero::new(AppModule).validate().unwrap();
    assert!(
        report.is_empty(),
        "nothing should be request-scoped:\n{report}"
    );
}
```

這個測試不建構任何東西，幾毫秒就跑完，建議每個專案都放一個。它也是檢查 scope 傳染的好地方：上例斷言沒有任何元件變成 request-scoped，所以有人不小心讓 controller 被傳染時，測試就會失敗。

## `TestApp` 與 override

`TestApp::new(root)` 相當於 Nest 的 `Test.createTestingModule()`。在 `build()` 之前可以替換任何 provider 或 enhancer：

<!-- snippet: tests/testing_guide.rs#fakes -->
```rust,ignore
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
```

<!-- snippet: tests/testing_guide.rs#override -->
```rust,ignore
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
```

| 方法 | 替換 |
|---|---|
| `override_provider::<T>(Arc<T>)` | 所有 `T` 的 provider；`T` 可以是 `dyn Trait` |
| `override_value(value)` | 同上，傳值即可 |
| `override_with(ProviderDef)` | 用任意 provider 定義替換（例如帶有依賴的 factory） |
| `override_guard::<G>(guard)` | 所有用到 guard `G` 的地方；替換品可以是任何實作 `Guard` 的型別 |
| `override_interceptor::<I>(interceptor)` | 同上，interceptor |
| `override_filter::<F>(filter)` | 同上，exception filter |
| `configure(\|hornero\| ...)` | 其他 `Hornero` 設定：全域 enhancer、layer、global prefix |

override 的規則：

- **全域替換**：不論 `T` 在哪個模組提供、被誰使用，都會被替換。
- **原本的依賴不再需要**：替換 `dyn TodoRepo` 之後，`SqlTodoRepo` 需要的資料庫連線不會被建構。
- **最後一個生效**：同一個 key 註冊多個 override 時，後註冊的取代先註冊的（可以在共用的 builder 上疊加一個 override），被取代的不會回報為錯誤。
- **替換品不參與 DI**：`override_guard` 收到的是已經建好的值，可以是沒有實作 `Injectable` 的普通 struct。
- **沒用到的 override 是錯誤**：override 了沒有任何模組提供的型別（例如型別打錯、或重構後 provider 不見了），`build()` 會回報 `unused override`：

<!-- snippet: tests/testing_guide.rs#unused-override -->
```rust,ignore
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
```

## 測試真正的 guard

不替換 guard，就能測試驗證流程本身：

<!-- snippet: tests/testing_guide.rs#real-guard -->
```rust,ignore
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
```

## Client 與回應

`app.client()` 回傳一個 in-process 的 `TestClient`：request 直接交給組好的 router（`tower::ServiceExt::oneshot`），不會開 port，所以測試可以平行執行。

| `TestClient` | |
|---|---|
| `get` / `post` / `put` / `patch` / `delete(path)` | 建立 request |
| `request(method, path)` | 任意 method |
| `with_header(name, value)` / `with_bearer(token)` | 之後每個 request 都帶上這個 header |

| `TestRequest` | |
|---|---|
| `header(name, value)` / `bearer(token)` | 這個 request 的 header |
| `json(&body)` | JSON body，並設定 `content-type` |
| `body(bytes)` | 原始 body |
| `.await` 或 `send().await` | 送出，取得 `TestResponse` |

| `TestResponse` | |
|---|---|
| `status()` / `headers()` / `header(name)` | 狀態碼與 headers |
| `text()` / `bytes()` / `json::<T>()` | body；`json` 失敗時會連同 body 一起 panic |
| `assert_status(expected)` | 狀態碼不符時連同 body 一起 panic，並回傳自己以便串接 |

<!-- snippet: tests/testing_guide.rs#errors -->
```rust,ignore
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
```

## 取得 singleton

`app.get::<T>()` 可以取得任何 singleton，用來檢查副作用。這是 service locator，只在測試中開放；一般程式碼請宣告依賴。

<!-- snippet: tests/testing_guide.rs#singletons -->
```rust,ignore
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
```

`get` 找不到時會 panic 並說明原因；`try_get` 則回傳 `Option`。request-scoped 與 transient 的 provider 無法這樣取得。

## 重用正式環境的 builder

把 `main` 用的 `Hornero` 設定（全域 guard、interceptor、filter、layer、global prefix）抽成函式，測試就能用 `TestApp::from_hornero` 套用完全相同的設定：

<!-- snippet: tests/testing_guide.rs#from-hornero -->
```rust,ignore
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
```

`examples/todo` 就是這樣做的：`lib.rs` 的 `hornero()` 同時給 `main.rs` 與 `tests/e2e.rs` 使用。

## 不透過容器的單元測試

provider 只是普通的 struct。單元測試寫在同一個檔案（或同一個模組）裡，就能直接建構它並傳入 fake：

<!-- snippet: tests/testing_guide.rs#unit -->
```rust,ignore
#[tokio::test]
async fn service_rejects_blank_titles() {
    // Providers are plain structs: a unit test can build one without the container.
    let service = TodoService {
        repo: Arc::new(InMemoryRepo::default()),
    };

    let err = service.create("   ".into()).await.unwrap_err();

    assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
```

## 其他建議

- **資料庫測試**用 SQLite 的 `sqlite::memory:`（見 [資料庫](12-database.md#測試)），或把 repository 放在 trait 後面，再替換成 in-memory 實作。
- **request-scoped 元件**也能用 override 替換；用 `app.scope_report()` 斷言哪些元件是 request-scoped。
- **生命週期**：`app.close().await` 會執行 destroy 與 shutdown hook，可以用來測試清理邏輯。
- **錯誤訊息**：`BootError` 的 `Display` 輸出很穩定，可以搭配 [insta](https://insta.rs) 做 snapshot 測試。

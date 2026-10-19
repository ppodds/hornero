# 8. 錯誤處理

hornero 有兩種錯誤：

- **`HttpError`**：request 期間的錯誤，會變成 HTTP 回應。
- **`BootError`**：啟動期間的錯誤，列出依賴圖、路由、建構或 hook 的所有問題。

## `HttpError`

`HttpError` 相當於 Nest 的 `HttpException`：狀態碼、給 client 看的訊息、選用的結構化細節，以及選用的內部原因（只寫 log，不會送給 client）。

| 建構方式 | 狀態碼 |
|---|---|
| `HttpError::bad_request(msg)` | 400 |
| `HttpError::unauthorized(msg)` | 401（預設帶有 `WWW-Authenticate: Bearer`） |
| `HttpError::forbidden(msg)` | 403 |
| `HttpError::not_found(msg)` | 404 |
| `HttpError::conflict(msg)` | 409 |
| `HttpError::unprocessable(msg)` | 422 |
| `HttpError::internal(msg)` | 500 |
| `HttpError::new(status, msg)` | 任意 |
| `HttpError::from_boxed(err)` / `HttpError::from(err)` | 500，`err` 成為內部原因 |

| Builder | 用途 |
|---|---|
| `.with_details(value)` | 結構化細節（任何 `Serialize`），渲染在 `errors` 欄位 |
| `.with_source(err)` | 內部原因：寫進 log，不會出現在回應裡 |
| `.with_header(name, value)` | 回應 header（同名會取代），例如 `HttpError::unauthorized(msg).with_header(WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"hornero\""))` |

header 跟著錯誤本身走：`render()` 會套用，自訂 exception filter 自己組回應時，用 `err.headers()` 取得並複製即可。`with_details` 的值序列化失敗時，`details` 維持空白並以 `tracing::warn!` 記錄。

預設渲染成 [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457) 的 `application/problem+json`：

```rust
use hornero::prelude::*;
use serde_json::json;

let err = HttpError::unprocessable("validation failed")
    .with_details(json!([{ "path": "title", "message": "length is lower than 1" }]));

assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
assert_eq!(
    err.to_problem(),
    json!({
        "type": "about:blank",
        "title": "Unprocessable Entity",
        "status": 422,
        "detail": "validation failed",
        "errors": [{ "path": "title", "message": "length is lower than 1" }]
    })
);
```

exception filter 處理完之後，**最終回應仍是 5xx** 的錯誤會用 `tracing::error!` 記錄，內容包含訊息與完整的 source chain；4xx 不會記錄。

## `?` 與 500

任何 `std::error::Error` 都可以用 `?` 轉成 `HttpError`，結果是**不透露細節的 500**：client 只看到 `internal server error`，原始錯誤寫進 log。資料庫錯誤、I/O 錯誤這類意外狀況，直接 `?` 就好：

```rust
use hornero::prelude::*;

#[controller("/numbers")]
struct NumberController;

#[routes]
impl NumberController {
    #[get("/{raw}")]
    fn parse(&self, Path(raw): Path<String>) -> Result<String, HttpError> {
        let number: u32 = raw.parse()?; // ParseIntError → 500
        Ok((number * 2).to_string())
    }
}

#[module(controllers = [NumberController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
assert_eq!(app.client().get("/numbers/21").await.text(), "42");

let res = app.client().get("/numbers/abc").await;
assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
let problem: serde_json::Value = res.json();
assert_eq!(problem["detail"], "internal server error"); // "invalid digit" 只出現在 log
# }
```

預期中的失敗（找不到、衝突、權限不足）也可以用 `?` 拋出，再由 exception filter 依錯誤型別轉成對應的狀態碼，見下一節。

## Domain error

service 回傳自己的錯誤型別，handler 用 `?` 拋出，exception filter 依型別轉成回應。這就是 Nest 的 `throw new TodoNotFound()` 搭配 `@Catch(TodoNotFound)`：

```rust
use std::fmt;

use hornero::prelude::*;

// domain 層：與 HTTP 無關
#[derive(Debug)]
enum TodoError {
    NotFound(u64),
    Duplicate(String),
}

impl fmt::Display for TodoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TodoError::NotFound(id) => write!(f, "todo {id} does not exist"),
            TodoError::Duplicate(title) => write!(f, "duplicate todo `{title}`"),
        }
    }
}

impl std::error::Error for TodoError {}

#[derive(Injectable)]
struct TodoService;

impl TodoService {
    async fn find(&self, id: u64) -> Result<String, TodoError> {
        match id {
            1 => Ok("write the docs".into()),
            _ => Err(TodoError::NotFound(id)),
        }
    }

    async fn create(&self, title: String) -> Result<String, TodoError> {
        if title == "write the docs" {
            return Err(TodoError::Duplicate(title));
        }
        Ok(title)
    }

    async fn export(&self) -> Result<String, std::io::Error> {
        Err(std::io::Error::other("disk full"))
    }
}

// HTTP 層：把 domain error 轉成回應
#[derive(Injectable)]
struct TodoErrorFilter;

impl ExceptionFilter for TodoErrorFilter {
    fn catches(&self, err: &HttpError) -> bool {
        err.is::<TodoError>() // 只接 TodoError，其他錯誤交給下一個 filter
    }

    async fn catch(&self, err: &HttpError, _ctx: &RouteContext) -> Response {
        let mapped = match err.downcast_ref::<TodoError>() {
            Some(TodoError::NotFound(id)) => HttpError::not_found(format!("todo {id} not found")),
            Some(TodoError::Duplicate(title)) => HttpError::conflict(format!("`{title}` already exists")),
            None => return err.render(),
        };
        mapped.into_response()
    }
}

#[controller("/todos")]
#[filters(TodoErrorFilter)]
struct TodoController {
    todos: Arc<TodoService>,
}

#[routes]
impl TodoController {
    #[get("/{id}")]
    async fn find(&self, Path(id): Path<u64>) -> Result<String, HttpError> {
        Ok(self.todos.find(id).await?) // TodoError 直接 `?`
    }

    #[post("/")]
    async fn create(&self, title: String) -> Result<(StatusCode, String), HttpError> {
        let title = self.todos.create(title).await?;
        Ok((StatusCode::CREATED, title))
    }

    #[get("/export")]
    async fn export(&self) -> Result<String, HttpError> {
        Ok(self.todos.export().await?) // io::Error：沒有 filter 接住
    }
}

#[module(controllers = [TodoController], providers = [TodoService])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let client = app.client();
assert_eq!(client.get("/todos/1").await.text(), "write the docs");

let res = client.get("/todos/7").await;
assert_eq!(res.status(), StatusCode::NOT_FOUND);
assert_eq!(res.json::<serde_json::Value>()["detail"], "todo 7 not found");

let res = client.post("/todos").body("write the docs").await;
assert_eq!(res.status(), StatusCode::CONFLICT);

let res = client.get("/todos/export").await;
assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
assert_eq!(res.json::<serde_json::Value>()["detail"], "internal server error"); // "disk full" 只寫進 log
# }
```

運作方式：

- `?` 把 `TodoError` 轉成 500 的 `HttpError`，原始錯誤保留為 source。`err.is::<T>()` 與 `err.downcast_ref::<T>()` 取回它。它們只看 `?` 轉換的那個錯誤本身，不會往 source chain 裡找：如果 `TodoError` 被包在另一個錯誤型別裡，請 catch 外層的型別。
- 發生錯誤時，hornero 依序查看 handler、controller、全域的 filter（同一層依註冊順序），交給**第一個 `catches` 回傳 `true` 的 filter**。`catches` 預設接住所有錯誤。
- 沒有任何 filter 接住時，錯誤維持原樣：不透露細節的 500，並寫進 log。被 filter 轉成 4xx 的錯誤不會留下 error log。

**filter 放在哪裡**：只有一個 controller 會遇到的錯誤，放在 controller（`#[controller]` 下方的 `#[filters(..)]`）。跨模組共用的 domain error，註冊成全域 filter。和接住所有錯誤的 filter（例如自訂格式的 `ProblemFilter`）一起使用時，**具體的先註冊**：

```rust
use hornero::prelude::*;
# #[derive(Injectable)]
# struct TodoErrorFilter;
# impl ExceptionFilter for TodoErrorFilter {
#     async fn catch(&self, err: &HttpError, _: &RouteContext) -> Response { err.render() }
# }
# #[derive(Injectable)]
# struct ProblemFilter;
# impl ExceptionFilter for ProblemFilter {
#     async fn catch(&self, err: &HttpError, _: &RouteContext) -> Response { err.render() }
# }
# #[module]
# struct AppModule;

Hornero::new(AppModule)
    .filter::<TodoErrorFilter>() // 只接 TodoError
    .filter::<ProblemFilter>()   // 其他所有錯誤
# ;
```

每個錯誤只會經過一個 filter，所以 `TodoErrorFilter` 的輸出不會再交給 `ProblemFilter`。需要統一格式時，把格式寫成一個函式，讓兩個 filter 都呼叫它。`examples/todo` 就是這樣做的：`src/todo/error.rs` 的 `TodoErrorFilter` 與 `src/common.rs` 的 `ProblemFilter` 共用 `problem()`。

> **為什麼不寫 `impl From<TodoError> for HttpError`？** `HttpError` 有 `impl<E: std::error::Error> From<E>`（`?` 能用在任何錯誤上就是靠它），所以替實作了 `Error` 的型別再寫 `From` 會衝突（E0119）。轉換交給 filter 就不需要它。

### 其他做法

**service 直接回傳 `HttpError`。** 小型應用最簡單的做法，但 service 會和 HTTP 綁在一起：

```rust
use hornero::prelude::*;
# struct Todo;
# struct TodoRepo;
# impl TodoRepo {
#     async fn find(&self, _id: i32) -> Result<Option<Todo>, std::io::Error> { Ok(None) }
# }
# struct TodoService {
#     repo: TodoRepo,
# }
# impl TodoService {
pub async fn get(&self, id: i32) -> Result<Todo, HttpError> {
    self.repo.find(id).await?.ok_or_else(|| HttpError::not_found(format!("todo {id} not found")))
}
# }
```

**domain error 實作 `IntoResponse`。** handler 直接回傳 domain error，轉換寫在錯誤型別旁邊，不需要註冊 filter：

```rust
use std::fmt;

use hornero::prelude::*;

#[derive(Debug)]
enum TodoError {
    NotFound(u64),
    Storage(std::io::Error),
}

impl fmt::Display for TodoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TodoError::NotFound(id) => write!(f, "todo {id} not found"),
            TodoError::Storage(err) => write!(f, "storage failure: {err}"),
        }
    }
}

impl std::error::Error for TodoError {}

impl IntoResponse for TodoError {
    fn into_response(self) -> Response {
        match self {
            TodoError::NotFound(id) => HttpError::not_found(format!("todo {id} not found")),
            TodoError::Storage(_) => HttpError::from(self), // 500，原因只寫 log
        }
        .into_response() // 經由 HttpError 渲染，所以 exception filter 也看得到
    }
}

#[controller("/todos")]
struct TodoController;

#[routes]
impl TodoController {
    #[get("/{id}")]
    fn find(&self, Path(id): Path<u64>) -> Result<String, TodoError> {
        match id {
            1 => Ok("write the docs".into()),
            2 => Err(TodoError::Storage(std::io::Error::other("disk full"))),
            _ => Err(TodoError::NotFound(id)),
        }
    }
}

#[module(controllers = [TodoController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let missing = app.client().get("/todos/7").await;
assert_eq!(missing.status(), StatusCode::NOT_FOUND);
assert_eq!(missing.json::<serde_json::Value>()["detail"], "todo 7 not found");

let broken = app.client().get("/todos/2").await;
assert_eq!(broken.status(), StatusCode::INTERNAL_SERVER_ERROR);
assert_eq!(broken.json::<serde_json::Value>()["detail"], "internal server error");
# }
```

handler 需要同時用 `?` 處理 domain error 與其他錯誤時，這種做法得在 `TodoError` 裡多一個包住其他錯誤的變體（例如上面的 `Storage`）；用 filter 則不需要。

## 錯誤回應的流向

`HttpError` 轉成回應時：

1. 依 `to_problem()` 渲染 problem+json並套用錯誤攜帶的 header（401 預設有 `WWW-Authenticate: Bearer`）。
2. 把錯誤本身附在回應的 extensions 上。
3. 回應往外傳到 exception filter：第一個 `catches` 它的 filter 重新渲染，見 [Request pipeline](05-request-pipeline.md#exception-filter)。
4. 在 hornero router 的最外層（所有全域 layer 之外），最終回應仍是 5xx 的錯誤寫進 log（包括 handler panic）。每個錯誤最多記錄一次。

以下回應**不是**來自 `HttpError`，所以 filter 看不到：

| 來源 | 回應 | 怎麼統一格式 |
|---|---|---|
| axum 內建 extractor 的 rejection（壞掉的 JSON、缺少的 query 參數） | 純文字，狀態碼由 extractor 決定 | 改用 `Valid<Json<T>>`（見 [驗證](11-validation.md)），或 `Rejection = HttpError` 的自訂 extractor |
| 沒有路由符合 | 404 problem+json | 已經是 problem+json，但不經過 filter |
| handler panic | 500 problem+json，panic 訊息只寫 log | 由 `CatchPanicLayer` 處理，它在全域 layer 與 middleware 的內側，回應仍會經過它們 |

## Bootstrap 錯誤

`Hornero::validate()`、`Hornero::build()`、`Hornero::listen()` 失敗時回傳 `BootError`。它會**一次列出所有問題**，而不是遇到第一個就停：

```rust
use hornero::prelude::*;

struct Database;
struct Cache;

#[derive(Injectable)]
struct UserService {
    db: Arc<Database>,
}

#[module(providers = [UserService], exports = [Cache])]
struct UsersModule;

let err = Hornero::new(UsersModule).validate().unwrap_err();
assert_eq!(
    err.to_string(),
    "failed to bootstrap application: 2 problems

[1] invalid export
    UsersModule exports `Cache`, but neither provides nor imports it.
    hint: register it in `providers`, or import the module that exports it.

[2] missing dependency
    UserService (in UsersModule) requires `Database`, but no provider of it is visible there.
    hint: no module provides `Database`; register it in the providers of UsersModule or of a module UsersModule imports.
"
);
```

檢查分三個階段，前一階段有問題就不會進到下一階段：依賴圖（可見性、循環、scope）→ 路由（衝突、語法）→ 建構與 hook（只有 `build` / `listen` 才會執行）。

在程式中可以用 `problems()` 逐一檢查，每個問題都是一個 `Problem`：

```rust
use hornero::Problem;
use hornero::prelude::*;

#[derive(Injectable)]
struct Orders {
    billing: Arc<Billing>,
}

#[derive(Injectable)]
struct Billing {
    orders: Arc<Orders>,
}

#[module(providers = [Orders, Billing])]
struct AppModule;

let err = Hornero::new(AppModule).validate().unwrap_err();
match &err.problems()[0] {
    Problem::CircularDependency { path } => assert_eq!(
        path.join(" -> "),
        "Orders (in AppModule) -> Billing (in AppModule) -> Orders (in AppModule)"
    ),
    other => panic!("unexpected problem: {other:?}"),
}
```

| `Problem` | 意義 | 常見修正 |
|---|---|---|
| `MissingDependency` | 依賴在該模組不可見 | 照 hint 註冊、export 或 import |
| `AmbiguousDependency` | 同一層有多個來源 | 在本地提供，或只 import 其中一個 |
| `DuplicateProvider` | 同一模組註冊了兩次同型別（且 qualifier 相同） | 給每個實例不同的 qualifier（`.qualified::<Q>()`），或用 newtype |
| `CircularDependency` | provider 互相依賴 | 抽出共用的 provider，或用 channel / 事件解耦 |
| `CircularImport` | 模組互相 import | 把共用部分移到第三個模組 |
| `InvalidExport` | export 了自己沒有、也沒 import 的型別 | 註冊它，或 import export 它的模組 |
| `InvalidModuleExport` | 轉出（`export_module`）了沒有 import 的模組 | import 那個模組 |
| `UndeclaredModuleIdentity` | 帶資料的模組被 import 了不只一次，卻沒有宣告身分 | 在 `fn identity()` 回傳 `Identity::Instance`、`Identity::key(..)` 或 `Identity::Type`，見 [模組的身分](03-modules.md#動態模組) |
| `DuplicateGlobalEnhancer` | 同一模組型別的多個實例宣告了同一個全域 enhancer | 改在只 import 一次的模組（例如根模組）宣告 |
| `ScopeViolation` | middleware / mount / 有 hook 的 provider 變成 request-scoped，或 pinned 的 provider 被傳染 | 見 [Scope](06-scopes.md#傳染的限制) |
| `UnusedOverride` | override 了沒有任何模組提供的型別 | 修正型別，或移除 override |
| `InvalidRoute` | 舊的 `:id` 語法、axum 不支援的 method、axum 拒絕的路由 | 改用 `{id}`；調整路徑 |
| `DuplicateRoute` | 兩個 handler 處理同一個 method 與路徑 | 調整路徑或前綴 |
| `Construction` | provider 建構失敗（factory 或建構函式回傳錯誤） | 檢查錯誤訊息，例如資料庫連不上、設定缺漏 |
| `Hook` | `on_module_init` 等 hook 失敗 | 檢查錯誤訊息 |

`Problem` 標記為 `#[non_exhaustive]`，`match` 時請保留 `_` 分支。

建議在測試中加入 `Hornero::new(AppModule).validate().unwrap()`：圖的問題在 `cargo test` 就會出現，而且失敗訊息就是上面這份報告。

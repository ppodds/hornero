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
| `HttpError::from_boxed(err)` | 500，`err` 成為內部原因 |
| `?` 或 `HttpError::from(err)` | 由處理 `err` 型別的 [exception filter](#domain-error) 決定，沒有就是 500；axum 自己的 rejection（`JsonRejection`、`QueryRejection` 等）保留原本的狀態碼 |

前面幾種是**回答**：錯誤本身就是要給 client 的回應，原樣渲染，exception filter 看不到。最後一列是**拋出**：`?` 帶著原始的 domain error 去找處理它型別的 filter，由 filter 決定狀態碼，原始錯誤保留為內部原因。

| Builder | 用途 |
|---|---|
| `.with_details(value)` | 結構化細節（任何 `Serialize`），渲染在 `errors` 欄位 |
| `.with_source(err)` | 內部原因：寫進 log，不會出現在回應裡 |
| `.with_header(name, value)` | 回應 header（同名會取代），例如 `HttpError::unauthorized(msg).with_header(WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"hornero\""))` |

header 跟著錯誤本身走：不論回應由預設格式或自訂的 [renderer](#統一錯誤格式) 產生，hornero 都會加上它們。handler 用 tuple 包住錯誤時也一樣：`([(SET_COOKIE, "session=; Max-Age=0")], HttpError::unauthorized(..))` 的 `Set-Cookie`、`(StatusCode::SERVICE_UNAVAILABLE, err)` 的狀態碼都會保留，和 axum 的行為相同。`with_details` 的值序列化失敗時，`details` 維持空白並以 `tracing::warn!` 記錄。

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

在一條路由裡，錯誤是**值**：guard、interceptor、extractor 與 handler 都回傳 `Result`，`HttpError` 一路往外傳，最後才渲染一次：

```text
guard、interceptor、extractor、handler   Err(HttpError)
        │  拋出的 domain error 離開每一層時，交給處理它型別的 exception filter
        ▼
exception filter      TodoError → HttpError::not_found(..)，原始錯誤保留為 source
        ▼
renderer              Hornero::error_renderer，預設 problem+json；加上錯誤攜帶的 header
        ▼
middleware、tower layer、5xx log
```

## `?` 與 500

任何 `std::error::Error` 都可以用 `?` 轉成 `HttpError`。沒有 filter 處理它的型別時，結果是**不透露細節的 500**：client 只看到 `internal server error`，原始錯誤寫進 log。資料庫錯誤、I/O 錯誤這類意外狀況，直接 `?` 就好：

```rust
use hornero::prelude::*;

#[controller("/numbers")]
struct NumberController;

#[routes]
impl NumberController {
    #[get("/{raw}")]
    fn parse(&self, Path(raw): Path<String>) -> Result<String, HttpError> {
        let number: u32 = raw.parse()?; // ParseIntError：沒有 filter 處理 → 500
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

log 會寫出錯誤的型別與完整的 source chain：

```text
ERROR unhandled core::num::error::ParseIntError: add an ExceptionFilter for it to choose the status status=500 Internal Server Error error=invalid digit found in string
```

這就是內建的 catch-all：框架不要求每條路由都掛上 filter，也不在啟動時檢查。預期中的失敗（找不到、衝突、權限不足）被當成 500 時，log 會指出是哪個型別，替它加一個 filter 即可，見下一節。

## Domain error

service 回傳自己的錯誤型別，handler 用 `?` 拋出，exception filter 依型別轉成 `HttpError`。這就是 Nest 的 `throw new TodoNotFound()` 搭配 `@Catch(TodoNotFound)`：

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

// HTTP 層：把 domain error 轉成 HTTP 錯誤
#[derive(Injectable)]
struct TodoErrorFilter;

impl ExceptionFilter for TodoErrorFilter {
    type Error = TodoError; // 相當於 @Catch(TodoError)

    async fn catch(&self, err: &TodoError, _ctx: &RouteContext) -> HttpError {
        match err {
            TodoError::NotFound(id) => HttpError::not_found(format!("todo {id} not found")),
            TodoError::Duplicate(title) => HttpError::conflict(format!("`{title}` already exists")),
        }
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
        Ok(self.todos.export().await?) // io::Error：沒有 filter 處理
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

- `?` 把 `TodoError` 轉成拋出的 `HttpError`，原始錯誤保留為 source。
- 錯誤離開 handler 時，hornero 依序查看 handler、controller、全域的 filter（同一層依註冊順序），交給**第一個 `Error` 型別相符的 filter**。guard、interceptor 與 extractor 用 `?` 拋出的錯誤也一樣。
- filter 回傳的 `HttpError` 是回答，原始錯誤成為它的 source：interceptor、renderer 與 middleware 裡，`err.is::<TodoError>()` 與 `err.downcast_ref::<TodoError>()` 一定取得到它。回傳值自己設的 source 會被原始錯誤取代，所以 filter 自己的失敗（例如查詢翻譯失敗）請在 filter 裡記錄。
- 每個錯誤最多轉換一次：filter 的回傳值不會再交給 filter，即使它是用 `HttpError::from` 建構的。
- 型別比對看的是 `?` 轉換的那個錯誤本身，不會往 source chain 裡找：如果 `TodoError` 被包在另一個錯誤型別裡，請替外層的型別寫 filter。
- 沒有 filter 處理時，錯誤成為不透露細節的 500 並寫進 log（見上一節）。被 filter 轉成 4xx 的錯誤不會留下 error log。
- filter 只決定**狀態碼與訊息**，回應的格式由 [renderer](#統一錯誤格式) 決定，所以每個 filter 都不必重複格式。

**filter 放在哪裡**：只有一個 controller 會遇到的錯誤，放在 controller（`#[controller]` 下方的 `#[filters(..)]`）。跨模組共用的 domain error（例如驗證身分失敗的 `AuthError`），註冊成全域 filter：`Hornero::filter::<F>()`，或模組 `providers` 裡的 `AppFilter<F>`（Nest 的 `APP_FILTER`）。每個 filter 只處理自己的型別，所以註冊順序只在同一個型別有多個 filter 時才有影響：較具體的（handler、controller）先處理。

> **為什麼不寫 `impl From<TodoError> for HttpError`？** `HttpError` 有 `impl<E: std::error::Error> From<E>`（`?` 能用在任何錯誤上就是靠它），所以替實作了 `Error` 的型別再寫 `From` 會衝突（E0119）。轉換交給 filter 就不需要它。

## 統一錯誤格式

錯誤的格式由 **renderer** 決定：一個 `fn(&HttpError, &RouteContext) -> Response`，用 `Hornero::error_renderer` 設定，整個應用只有一個。沒有設定時渲染成 problem+json。

renderer 收到的是 filter 轉換之後的錯誤，負責路由上的**每一個**錯誤：handler、guard、interceptor、extractor 回傳的錯誤，以及 axum extractor 的 rejection（壞掉的 JSON、無法解析的路徑參數）。錯誤攜帶的 header 由 hornero 加上，renderer 不必自己複製：

```rust
use hornero::prelude::*;
use serde_json::json;

/// 應用的錯誤格式
fn house_format(err: &HttpError, ctx: &RouteContext) -> Response {
    let body = json!({ "error": err.message(), "route": ctx.path() });
    (err.status(), Json(body)).into_response()
}

#[derive(Injectable)]
struct TokenGuard;

impl Guard for TokenGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        match ctx.parts.headers.get("authorization") {
            Some(_) => Ok(()),
            None => Err(HttpError::unauthorized("missing token")),
        }
    }
}

#[derive(serde::Deserialize)]
struct NewNote {
    title: String,
}

#[controller("/notes")]
#[guards(TokenGuard)]
struct NoteController;

#[routes]
impl NoteController {
    #[post("/")]
    fn create(&self, Json(note): Json<NewNote>) -> String {
        note.title
    }
}

#[module(controllers = [NoteController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let hornero = Hornero::new(AppModule).error_renderer(house_format);
let app = hornero_testing::TestApp::from_hornero(hornero).build().await.unwrap();
let client = app.client();

// guard 的錯誤：格式來自 renderer，WWW-Authenticate 來自錯誤本身
let res = client.post("/notes").await;
assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
assert_eq!(res.header("www-authenticate"), Some("Bearer"));
assert_eq!(res.json::<serde_json::Value>(), json!({ "error": "missing token", "route": "/notes" }));

// axum extractor 的 rejection：保留 axum 的狀態碼與訊息
let res = client
    .post("/notes")
    .bearer("token")
    .header("content-type", "application/json")
    .body("{")
    .await;
assert_eq!(res.status(), StatusCode::BAD_REQUEST);
let body: serde_json::Value = res.json();
assert!(body["error"].as_str().unwrap().starts_with("Failed to parse the request body as JSON"));
# }
```

axum 的 rejection 怎麼變成錯誤：

- axum 內建 extractor 的 rejection 保留它的狀態碼與訊息。在 [`Extractor`](06-scopes.md#需要-provider-的-extractor) 裡對它用 `?`（例如 `Query::<Page>::from_request_parts(parts, &()).await?`）也一樣是 400，不會變成 500；它和 handler 參數的 rejection 一樣是回答，filter 看不到。
- `Rejection = HttpError` 的 extractor（包括 `hornero-validation` 的 `Valid`）回傳的就是那個 `HttpError`。
- 其他 rejection 取它的狀態碼、文字與 header（同名的多個值都保留），例如 `(StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, "Bearer")], "missing bearer token")`。
- 不是 4xx / 5xx 的 rejection（例如導向登入頁的 `Redirect`）不是錯誤，原樣送出。

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
            TodoError::Storage(_) => HttpError::from_boxed(self.into()), // 500，原因只寫 log
        }
        .into_response() // 經由 HttpError 渲染，所以 renderer 與 interceptor 也拿得到這個錯誤
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

路由上的錯誤：

1. guard、interceptor、extractor 或 handler 回傳 `Err(HttpError)`。
2. 拋出的 domain error 離開 handler、extractor 或 interceptor 時，由處理它型別的 filter 轉換，所以外層 interceptor 看到的已經是 client 會收到的狀態碼；guard 的錯誤在渲染前轉換。見 [Request pipeline](05-request-pipeline.md#exception-filter)。
3. 錯誤渲染一次：renderer，或預設的 problem+json。hornero 加上錯誤攜帶的 header（401 預設有 `WWW-Authenticate: Bearer`），並把錯誤附在回應的 extensions 上。
4. 回應往外經過模組與全域的 middleware、tower layer。middleware 用 `HttpError::in_response(&response)` 讀得到錯誤。
5. 在 hornero router 的最外層（所有全域 layer 之外），最終回應仍是 5xx 的錯誤寫進 log（包括 handler panic）。每個錯誤最多記錄一次。

renderer 只負責路由上的錯誤。以下回應不經過路由，維持 problem+json：

| 來源 | 回應 |
|---|---|
| 沒有路由符合 | 404 |
| 路徑符合但 method 不符 | 405，保留 `Allow` header |
| handler panic | 500，panic 訊息只寫 log。由 `CatchPanicLayer` 處理，它在全域 layer 與 middleware 的內側，回應仍會經過它們 |
| middleware 回傳的 `HttpError` | 依 `HttpError` 的 `IntoResponse` 渲染 |
| 關機逾時被取消的 request | 503 |

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
| `AmbiguousReexport` | `export_module` 轉出的模組提供了同一型別的不同實例 | 在轉出的模組提供並 export 它（會遮蔽轉出的），或只從其中一個來源轉出；來源是同一模組型別的幾個實例時，只 import 一次並共用，或讓各實例的 provider 有自己的型別（例如泛型 tag） |
| `DuplicateProvider` | 同一模組註冊了兩次同型別（且 qualifier 相同） | 給每個實例不同的 qualifier（`.qualified::<Q>()`），或用 newtype |
| `CircularDependency` | provider 互相依賴 | 抽出共用的 provider，或用 channel / 事件解耦 |
| `CircularImport` | 模組互相 import | 把共用部分移到第三個模組 |
| `InvalidExport` | export 了自己沒有、也沒 import 的型別 | 註冊它，或 import export 它的模組 |
| `InvalidModuleExport` | 轉出（`export_module`）了沒有 import 的模組 | import 那個模組 |
| `UndeclaredModuleIdentity` | 帶資料的模組被 import 了不只一次，卻沒有宣告身分 | 在 `fn identity()` 回傳 `Identity::Instance`、`Identity::key(..)` 或 `Identity::Type`，見 [模組的身分](03-modules.md#動態模組) |
| `DuplicateGlobalEnhancer` | 同一模組型別的多個實例宣告了同一個全域 enhancer | 改在只 import 一次的模組（例如根模組）宣告 |
| `ScopeViolation` | 有 hook 的 provider 或 controller 宣告成 transient | 改成 singleton，見 [Scope 的限制](06-scopes.md#scope-的限制) |
| `UnusedOverride` | override 了沒有任何模組提供的型別 | 修正型別，或移除 override |
| `InvalidRoute` | 舊的 `:id` 語法、axum 不支援的 method、axum 拒絕的路由 | 改用 `{id}`；調整路徑 |
| `DuplicateRoute` | 兩個 handler 處理同一個 method 與路徑 | 調整路徑或前綴 |
| `Construction` | provider 建構失敗（factory 或建構函式回傳錯誤） | 檢查錯誤訊息，例如資料庫連不上、設定缺漏 |
| `Hook` | `on_module_init` 等 hook 失敗 | 檢查錯誤訊息 |

`Problem` 標記為 `#[non_exhaustive]`，`match` 時請保留 `_` 分支。

建議在測試中加入 `Hornero::new(AppModule).validate().unwrap()`：圖的問題在 `cargo test` 就會出現，而且失敗訊息就是上面這份報告。

# 4. Controller 與路由

controller 是一組路由，背後是一個可注入的 struct。它和其他 provider 一樣由容器建構，依賴寫在欄位上；handler 是一般的方法，參數是 axum extractor。

## 一個完整的 controller

```rust
use std::collections::BTreeMap;
use std::sync::Mutex;

use hornero::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct Note {
    id: u64,
    text: String,
}

#[derive(Deserialize)]
struct NewNote {
    text: String,
}

#[derive(Deserialize)]
struct ListQuery {
    contains: Option<String>,
}

#[derive(Injectable)]
struct NoteStore {
    #[inject(default)]
    notes: Mutex<BTreeMap<u64, Note>>,
}

#[controller("/notes")]
struct NoteController {
    store: Arc<NoteStore>,
}

#[routes]
impl NoteController {
    #[get("/")]
    fn list(&self, Query(query): Query<ListQuery>) -> Json<Vec<Note>> {
        let notes = self.store.notes.lock().unwrap();
        let matches = |note: &&Note| match &query.contains {
            Some(text) => note.text.contains(text.as_str()),
            None => true,
        };
        Json(notes.values().filter(matches).cloned().collect())
    }

    #[get("/{id}")]
    async fn find_one(&self, Path(id): Path<u64>) -> Result<Json<Note>, HttpError> {
        let notes = self.store.notes.lock().unwrap();
        let note = notes.get(&id).cloned();
        note.map(Json).ok_or_else(|| HttpError::not_found(format!("note {id} not found")))
    }

    #[post("/")]
    async fn create(&self, Json(input): Json<NewNote>) -> (StatusCode, Json<Note>) {
        let mut notes = self.store.notes.lock().unwrap();
        let note = Note { id: notes.len() as u64 + 1, text: input.text };
        notes.insert(note.id, note.clone());
        (StatusCode::CREATED, Json(note))
    }

    #[delete("/{id}")]
    async fn remove(&self, Path(id): Path<u64>) -> StatusCode {
        match self.store.notes.lock().unwrap().remove(&id) {
            Some(_) => StatusCode::NO_CONTENT,
            None => StatusCode::NOT_FOUND,
        }
    }
}

#[module(controllers = [NoteController], providers = [NoteStore])]
struct NotesModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
// 用 hornero-testing 試打看看
let app = hornero_testing::TestApp::new(NotesModule).build().await.unwrap();
let client = app.client();

let created = client.post("/notes").json(&serde_json::json!({ "text": "buy milk" })).await;
assert_eq!(created.status(), StatusCode::CREATED);

let note: Note = client.get("/notes/1").await.json();
assert_eq!(note.text, "buy milk");

let found: Vec<Note> = client.get("/notes?contains=milk").await.json();
assert_eq!(found.len(), 1);

let missing = client.get("/notes/9").await;
assert_eq!(missing.status(), StatusCode::NOT_FOUND);
assert_eq!(missing.header("content-type"), Some("application/problem+json"));
# }
```

## `#[controller]`

```rust
use hornero::prelude::*;
# #[derive(Injectable)]
# pub struct TodoService;
# #[derive(Injectable)]
# struct AuthGuard;
# impl Guard for AuthGuard {
#     async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> { Ok(()) }
# }
# #[derive(Injectable)]
# struct Timing;
# impl Interceptor for Timing {
#     async fn intercept(&self, _: &RouteContext, req: Request, next: Next) -> Response { next.run(req).await }
# }
# #[derive(Injectable)]
# struct ProblemFilter;
# impl ExceptionFilter for ProblemFilter {
#     async fn catch(&self, err: &HttpError, _: &RouteContext) -> Response { err.render() }
# }
# struct Public;
# use hornero::axum::extract::Request;
# use hornero::axum::middleware::Next;

#[controller("/todos")]
#[guards(AuthGuard)]
#[interceptors(Timing)]
#[filters(ProblemFilter)]
#[meta(Public)]
pub struct TodoController {
    todos: Arc<TodoService>,
}
# #[routes]
# impl TodoController {}
```

- 參數是路由前綴，可以省略（`#[controller]`）。
- 下方疊加的 `#[guards(..)]`、`#[interceptors(..)]`、`#[filters(..)]` 套用到這個 controller 的每一條路由，`#[meta(..)]` 設定 controller 層級的 metadata，對應 Nest 的 `@UseGuards`、`@UseInterceptors`、`@UseFilters` 與 `@SetMetadata`。詳見 [Request pipeline](05-request-pipeline.md)。
- 這些 attribute 必須寫在 `#[controller]` **下方**（Nest 的慣例也是 `@Controller` 寫在最上面）；寫在上方會得到明確的編譯錯誤。同一種 attribute 可以寫多次，也可以一次列多個：`#[guards(AuthGuard, RolesGuard)]`，依由上到下、由左到右的順序執行。
- 也可以寫成參數：`#[controller("/todos", guards = [AuthGuard], meta = [Public])]`，效果相同。兩種寫法並用時，參數裡的先執行。
- struct 的規則和 `#[derive(Injectable)]` 相同：`Arc<T>` 欄位是依賴，其他欄位要加 `#[inject(default)]` 或 `#[inject(with = ...)]`。
- 要設定 scope 時，在 struct 上加 `#[injectable(scope = request)]` 或 `#[inject(scope = request)]`。通常不需要：controller 會依依賴自動變成 request-scoped（見 [Scope](06-scopes.md)）。

## `#[routes]`

`#[routes]` 寫在 controller 的 inherent `impl` 區塊上。標了路由屬性的方法會變成路由，其他方法（helper）保持原樣。

| 屬性 | HTTP method |
|---|---|
| `#[get("/path")]` | GET |
| `#[post("/path")]` | POST |
| `#[put("/path")]` | PUT |
| `#[patch("/path")]` | PATCH |
| `#[delete("/path")]` | DELETE |
| `#[head("/path")]` | HEAD |
| `#[options("/path")]` | OPTIONS |
| `#[route("METHOD", "/path")]` | 其他 method，例如 `#[route("TRACE", "/debug")]`；method 字串不是合法的 token（例如 `"GE T"`）會是編譯錯誤 |

同一個方法上還可以加 `#[guards(...)]`、`#[interceptors(...)]`、`#[filters(...)]`、`#[meta(...)]`，只作用在這條路由。寫法和 controller 層級相同。

每個 controller 只能有一個 `#[routes]` 區塊，因為它會產生該 controller 的 `impl Controller`。

## 路徑

路徑使用 axum 0.8 的語法：

| 寫法 | 意義 | 範例 |
|---|---|---|
| `/users/{id}` | 一個路徑參數 | `Path(id): Path<u64>` |
| `/users/{id}/posts/{post}` | 多個參數 | `Path((id, post)): Path<(u64, u64)>` |
| `/files/{*rest}` | 萬用字元，匹配剩下的所有路徑 | `Path(rest): Path<String>` |

完整路徑是「global prefix + controller 前綴 + 路由路徑」，多餘或缺少的 `/` 會被正規化：

| global prefix | controller | 路由 | 結果 |
|---|---|---|---|
| | `"/notes"` | `"/"` | `/notes` |
| | `"/notes"` | `"{id}"` | `/notes/{id}` |
| `"/api"` | `"notes/"` | `"/{id}"` | `/api/notes/{id}` |
| `"/api"` | （無） | `"/health"` | `/api/health` |

global prefix 在 builder 上設定，也會套用到 mount 進來的 router：

```rust
use hornero::prelude::*;
# #[module]
# struct AppModule;

Hornero::new(AppModule).global_prefix("/api")
# ;
```

以下情況會在啟動時回報錯誤，而不是讓 axum 在執行期 panic：

- 兩條路由的 method 與路徑形狀相同（`/todos/{id}` 和 `/todos/{key}` 視為相同）→ `duplicate route`
- 使用舊語法 `:id` 或 `*rest` → `invalid route`
- axum 無法比對的自訂 method（例如 `PURGE`，它是合法的 token，所以能通過編譯）→ `invalid route`

```rust
use hornero::prelude::*;

#[controller("/users")]
struct UserController;

#[routes]
impl UserController {
    #[get("/:id")]
    async fn find(&self) -> &'static str {
        "user"
    }
}

#[module(controllers = [UserController])]
struct AppModule;

let err = Hornero::new(AppModule).validate().unwrap_err();
assert!(err.to_string().contains(
    "UserController::find uses the path segment `:id`; axum 0.8 expects `{param}` and `{*rest}`"
));
```

## Handler 的簽名

- **receiver**：`&self`，或 `self: Arc<Self>`（要把 controller 移進 `tokio::spawn` 時使用）。不支援 `&mut self`，因為同一個 controller 實例會同時服務多個 request。
- **同步或非同步都可以**：沒有 `.await` 的 handler 可以寫成一般的 `fn`。
- **其餘參數都是 axum extractor**，最多 12 個。只有最後一個可以讀取 body（`Json<T>`、`String`、`Bytes`、`Form<T>`……），其他的必須實作 `FromRequestParts`。
- **回傳值是任何 `IntoResponse`**。
- handler 不能有泛型參數。

```rust
use hornero::prelude::*;

#[derive(Injectable)]
struct SearchIndex;

impl SearchIndex {
    async fn rebuild(&self) {}
}

#[controller("/admin")]
struct AdminController {
    index: Arc<SearchIndex>,
}

#[routes]
impl AdminController {
    // 回應 202 之後在背景重建索引
    #[post("/reindex")]
    async fn reindex(self: Arc<Self>) -> StatusCode {
        tokio::spawn(async move { self.index.rebuild().await });
        StatusCode::ACCEPTED
    }

    // 同步 handler
    #[get("/ping")]
    fn ping(&self) -> &'static str {
        "pong"
    }

    // 不是路由：沒有路由屬性的方法保持原樣
    fn helper(&self) -> usize {
        42
    }
}
```

## Extractor

handler 參數就是 axum extractor，axum 生態系裡的 extractor 都能直接使用：

| Extractor | 取得 |
|---|---|
| `Path<T>` | 路徑參數 |
| `Query<T>` | query string（`T: Deserialize`） |
| `Json<T>` | JSON body |
| `HeaderMap`、`Method`、`Uri` | request head |
| `Extension<T>` | middleware 或 guard 放進 extensions 的資料 |
| `String`、`Bytes` | 原始 body |
| `Valid<Json<T>>` 等 | 驗證過的資料，見 [驗證](11-validation.md) |
| 自訂型別 | 實作 `FromRequestParts` 或 `FromRequest` |

hornero **不使用 axum 的 `State`**：controller 需要的東西都透過 DI 注入，router 的 state 型別固定是 `()`。

自訂 extractor 的 `Rejection` 建議用 `HttpError`，錯誤回應才會是一致的 problem+json，也會經過 exception filter：

```rust
use hornero::axum::extract::FromRequestParts;
use hornero::axum::http::request::Parts;
use hornero::prelude::*;

struct ApiVersion(u8);

impl<S: Send + Sync> FromRequestParts<S> for ApiVersion {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, HttpError> {
        match parts.headers.get("x-api-version") {
            None => Ok(ApiVersion(1)),
            Some(value) => value
                .to_str()
                .ok()
                .and_then(|value| value.parse().ok())
                .map(ApiVersion)
                .ok_or_else(|| HttpError::bad_request("invalid x-api-version header")),
        }
    }
}

#[controller("/version")]
struct VersionController;

#[routes]
impl VersionController {
    #[get("/")]
    fn version(&self, ApiVersion(version): ApiVersion) -> String {
        format!("v{version}")
    }
}

#[module(controllers = [VersionController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
assert_eq!(app.client().get("/version").await.text(), "v1");
let bad = app.client().get("/version").header("x-api-version", "two").await;
assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
# }
```

需要「已驗證身分」這類資料時，把它做成 extractor：handler 參數裡出現 `user: AuthUser`，本身就證明這個 request 已經通過驗證。做法見 [Request pipeline](05-request-pipeline.md#guard)。

## 回傳值

任何 `IntoResponse` 都可以：

| 回傳型別 | 回應 |
|---|---|
| `String`、`&'static str` | 200，`text/plain` |
| `Json<T>` | 200，`application/json` |
| `StatusCode` | 只有狀態碼 |
| `(StatusCode, Json<T>)` | 自訂狀態碼加 body |
| `([(HeaderName, &str); N], T)` | 加上 headers |
| `Result<T, HttpError>` | 成功時是 `T`，失敗時是 problem+json，見 [錯誤處理](08-errors.md) |
| `Response` | 完全自訂 |

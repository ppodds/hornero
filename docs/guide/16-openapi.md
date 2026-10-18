# 16. OpenAPI：hornero-openapi

`hornero-openapi` 從應用的路由產生 [OpenAPI 3.1](https://spec.openapis.org/oas/v3.1.0) 文件，並提供 Swagger UI，相當於 `@nestjs/swagger`。它建立在 [utoipa](https://docs.rs/utoipa) 之上，但不需要像 utoipa 那樣替每個 handler 寫 `#[utoipa::path(...)]`：路徑、參數、body、回應與身分驗證，大多直接從 handler 的簽名與 guard 推導出來。

```toml
[dependencies]
hornero = { path = "../hornero/crates/hornero", features = ["swagger-ui"] }
utoipa = "6"
serde = { version = "1", features = ["derive"] }
```

- `swagger-ui` feature 包含 `openapi`，並在 `/docs` 提供 Swagger UI；只需要 JSON 文件時開 `openapi` 即可。Swagger UI 的檔案在編譯時就嵌進執行檔，不需要網路。
- `utoipa` 必須加在你自己的依賴裡：`#[derive(ToSchema)]` 產生的程式碼會引用 `utoipa`，和 garde 的情況一樣。版本必須是 6。
- 需要 Rust 1.88 以上（utoipa 6 的要求）。

## 掛上文件

在根模組 import `OpenApiModule::for_root`，DTO 加上 `#[derive(ToSchema)]`：

```rust
use hornero::openapi::{OpenApiConfig, OpenApiModule, ToSchema};
use hornero::prelude::*;
use serde::{Deserialize, Serialize};

/// 一筆待辦事項。
#[derive(Serialize, ToSchema)]
struct Todo {
    id: u64,
    title: String,
}

#[derive(Deserialize, ToSchema)]
struct CreateTodo {
    title: String,
}

#[controller("/todos")]
struct TodoController;

#[routes]
impl TodoController {
    /// 列出所有待辦事項。
    #[get("/")]
    async fn list(&self) -> Json<Vec<Todo>> {
        Json(vec![Todo { id: 1, title: "寫文件".into() }])
    }

    /// 新增一筆待辦事項。
    #[post("/")]
    #[api_response(201)]
    async fn create(&self, Json(input): Json<CreateTodo>) -> (StatusCode, Json<Todo>) {
        (StatusCode::CREATED, Json(Todo { id: 2, title: input.title }))
    }
}

#[module(controllers = [TodoController])]
struct TodoModule;

#[module(imports = [
    OpenApiModule::for_root(OpenApiConfig::new("Todo API", "1.0")),
    TodoModule,
])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let doc: serde_json::Value = app.client().get("/docs/openapi.json").await.json();

let create = &doc["paths"]["/todos"]["post"];
assert_eq!(create["summary"], "新增一筆待辦事項。");
assert_eq!(
    create["requestBody"]["content"]["application/json"]["schema"]["$ref"],
    "#/components/schemas/CreateTodo"
);
assert_eq!(
    create["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
    "#/components/schemas/Todo"
);
assert_eq!(doc["components"]["schemas"]["Todo"]["description"], "一筆待辦事項。");

// Swagger UI
assert_eq!(app.client().get("/docs/").await.status(), StatusCode::OK);
# }
```

啟動後打開 `http://localhost:3000/docs` 就是 Swagger UI，`/docs/openapi.json` 是文件本身。兩者都在 `Hornero::global_prefix` 之下：前綴是 `/api` 時，網址是 `/api/docs`。

文件在啟動時由[路由表](../api/hornero-core.md#routesrouteinfo-與-enhancerinfo)產生一次，所以每個模組的 controller 路由都會出現，路徑也包含 global prefix 與 controller prefix。`mount_router` 與 `mount` 掛上的 axum router 不在文件裡，hornero 不知道它們的路由。

## 從簽名推導什麼

`#[routes]` 在編譯時讀 handler 的參數與回傳型別：

| handler 的寫法 | 文件中的結果 |
|---|---|
| `Path(id): Path<u64>` | path 參數，名稱取自路徑中的 `{id}` |
| `Path((a, b)): Path<(String, u32)>` | 依序對應路徑中的參數 |
| `Path(p): Path<Params>` | `Params` 的欄位，`Params` 需要 `#[derive(IntoParams)]` 或 `#[derive(ToSchema)]` |
| `Query(q): Query<Filter>` | query 參數，同上；`Option` 欄位不是必填 |
| `Json(body): Json<T>` | `application/json` 的 request body |
| `Form(body): Form<T>` | `application/x-www-form-urlencoded` 的 request body |
| `Valid(..): Valid<E>` | `E` 的推導結果，加上 422 回應 |
| 回傳 `Json<T>` | 200，`application/json` |
| 回傳 `(StatusCode, Json<T>)`、`(StatusCode, HeaderMap, Json<T>)` | tuple 最後一個元素決定 body；狀態碼要用 `#[api_response]` 說明 |
| 回傳 `Result<T, E>` | `T` 的推導結果，加上 `default` 回應（problem+json） |
| 回傳 `String`、`&'static str` / `Html<_>` | `text/plain` / `text/html` 字串 |
| 回傳 `StatusCode`、`()`、`Response`、`impl IntoResponse` | 沒有 body 的 200 |
| doc comment | 第一段是 `summary`，其餘是 `description` |
| `#[deprecated]` | `deprecated: true` |
| controller 名稱 | tag：`TodoController` 是 `Todo` |
| handler 名稱 | `operationId`：`TodoController_create` |

其他參數（`AuthUser` 這類自訂 extractor、`HeaderMap`、`Extension`）不影響文件。

body 與參數的型別這樣對應到 schema：

| Rust 型別 | Schema |
|---|---|
| 有 `#[derive(ToSchema)]` 的 struct 或 enum | `$ref` 到 `components.schemas` 裡的同名 schema |
| 泛型，例如 `Page<Todo>` | 直接展開（泛型的各種實例共用一個名稱，不能放進 components） |
| `Vec<T>`、`[T]`、`HashSet<T>`、`BTreeSet<T>` | `array` |
| `Option<T>` | `T` 或 `null` |
| `HashMap<K, V>`、`BTreeMap<K, V>` | 以 `additionalProperties` 描述值的 object |
| `Box<T>`、`Arc<T>`、`Rc<T>`、`Cow<T>`、`&T` | 同 `T` |
| 數字、`bool`、`String`、`char` | 對應的基本型別 |
| `Uuid`、`DateTime`、`NaiveDate`、`Url`… | 帶 `format` 的字串（除非它自己實作了 `ToSchema`） |
| `serde_json::Value` | 任何值 |

推導只看語法，所以有兩個限制：

- **型別別名與自訂 extractor 認不出來**。`type Body = Json<Todo>` 或自製的 `Multipart` extractor，用 `#[api_body]` 說明。
- **garde 的規則不會變成 schema 的限制**。`#[garde(length(min = 1))]` 不會出現在文件裡；需要的話，用 utoipa 的 `#[schema(min_length = 1)]` 再寫一次。

## 補充推導不出來的部分

| 屬性 | 位置 | 作用 |
|---|---|---|
| `#[api_response(status, description = "..", body = T, content_type = "..")]` | handler，或 `#[controller]` 下方 | 一個回應；除了狀態碼都可以省略 |
| `#[api_body(T, content_type = "..")]` | handler | request body，覆蓋推導的結果；`content_type` 預設 `application/json` |
| `#[api_tags("A", "B")]` | handler，或 `#[controller]` 下方 | 取代預設的 tag |
| `#[api_exclude]` | handler，或 `#[controller]` 下方 | 不出現在文件裡 |

沒有寫 `body` 的 `#[api_response]` 依狀態碼決定 body：

- **2xx**（204 除外）沿用從回傳型別推導的 body，所以 `#[api_response(201)]` 就足以把 `(StatusCode, Json<Todo>)` 說清楚。
- **4xx、5xx** 是 hornero 的錯誤格式：`application/problem+json` 的 `ProblemDetails` schema（見 [錯誤處理](08-errors.md)）。
- 只要宣告了任何 2xx，就不會再自動加上 200。
- `description` 預設是狀態碼的標準說明，例如 `Not Found`。

寫在 `#[controller]` 下方的 `#[api_response]` 套用到每一條路由，handler 自己寫了同一個狀態碼時以 handler 為準。

```rust
use hornero::openapi::ToSchema;
use hornero::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, ToSchema)]
struct Upload {
    name: String,
}

#[derive(Serialize, ToSchema)]
struct Stored {
    names: Vec<String>,
}

#[controller("/files")]
#[api_tags("Files")]
#[api_response(401, description = "需要登入")]
struct FileController;

#[routes]
impl FileController {
    /// 上傳檔案。
    #[post("/")]
    #[api_body(Upload, content_type = "multipart/form-data")]
    #[api_response(200, body = Stored, description = "已儲存的檔名")]
    #[api_response(413)]
    async fn upload(&self) -> Response {
        StatusCode::OK.into_response()
    }

    #[get("/debug")]
    #[api_exclude]
    async fn debug(&self) -> &'static str {
        "ok"
    }
}

#[module(controllers = [FileController])]
struct AppModule;

let routes = Hornero::new(AppModule).routes().unwrap();
let doc = hornero::openapi::document(&routes, &hornero::openapi::OpenApiConfig::new("Files", "1.0"));
let doc = serde_json::to_value(&doc).unwrap();
let upload = &doc["paths"]["/files"]["post"];
assert_eq!(upload["tags"], serde_json::json!(["Files"]));
assert!(upload["requestBody"]["content"]["multipart/form-data"].is_object());
assert_eq!(upload["responses"]["401"]["description"], "需要登入");
assert_eq!(upload["responses"]["413"]["description"], "Payload Too Large");
assert!(doc["paths"]["/files/debug"].is_null());
```

## 身分驗證：從 guard 推導

替負責驗證身分的 guard 實作 `ApiSecurity`，經過它的每一條路由（全域的 `Hornero::guard`、模組的 `AppGuard`、controller 或 handler 上的 `#[guards]`）都會在文件裡要求這個 security scheme，Swagger UI 也會出現「Authorize」按鈕。不需要像 Nest 那樣在每個 controller 上重複 `@ApiBearerAuth()`：

```rust
use hornero::openapi::{ApiSecurity, SecurityScheme};
use hornero::prelude::*;
use hornero::RouteInfo;

/// 標記不需要登入的路由：`#[meta(Public)]`。
struct Public;

#[derive(Injectable)]
struct AuthGuard;

impl Guard for AuthGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        if ctx.route.meta::<Public>().is_some() {
            return Ok(());
        }
        // 檢查 bearer token……
        Ok(())
    }
}

impl ApiSecurity for AuthGuard {
    fn scheme() -> (&'static str, SecurityScheme) {
        ("bearer", SecurityScheme::bearer_jwt())
    }

    // 和 can_activate 的判斷一致：Public 的路由不需要 token。
    fn required(route: &RouteInfo) -> bool {
        route.meta::<Public>().is_none()
    }
}

#[controller("/me")]
struct MeController;

#[routes]
impl MeController {
    #[get("/")]
    async fn me(&self) -> &'static str {
        "alice"
    }

    #[get("/ping")]
    #[meta(Public)]
    async fn ping(&self) -> &'static str {
        "pong"
    }
}

#[module(controllers = [MeController], providers = [AppGuard<AuthGuard>])]
struct AppModule;

let routes = Hornero::new(AppModule).routes().unwrap();
let doc = hornero::openapi::document(&routes, &hornero::openapi::OpenApiConfig::new("Me", "1.0"));
let doc = serde_json::to_value(&doc).unwrap();
assert_eq!(doc["paths"]["/me"]["get"]["security"], serde_json::json!([{ "bearer": [] }]));
assert!(doc["paths"]["/me/ping"]["get"]["security"].is_null());
assert_eq!(doc["components"]["securitySchemes"]["bearer"]["bearerFormat"], "JWT");
```

- `scheme()` 回傳 scheme 的名稱與內容。名稱相同的 guard 共用一個 scheme。
- 一條路由經過好幾個實作 `ApiSecurity` 的 guard 時，全部都是必要條件。
- `scopes(route)` 回傳 OAuth2 / OpenID Connect 需要的 scope，預設沒有。
- `SecurityScheme` 提供 `bearer()`、`bearer_jwt()`、`basic()`、`api_key_header(name)`、`api_key_query(name)`、`api_key_cookie(name)`；OAuth2 這類較複雜的 scheme，從 utoipa 的 `SecurityScheme` 轉換即可。

guard 的 scheme 是 `#[derive(Injectable)]` 記錄下來的：開啟 `openapi` feature 時，derive 會在 [`Injectable::describe`](../api/hornero-core.md#trait-injectable) 裡檢查型別是否實作了 `ApiSecurity`。手寫 `Injectable` 的 guard 要自己呼叫 `hornero::openapi::describe_security::<Self>(meta)`，見 [API 參考](../api/hornero-openapi.md#describe_security)。

## 推導不出來的時候

型別沒有實作 `ToSchema`（或 query 型別沒有實作 `IntoParams` 也沒有 `ToSchema`）不會讓編譯失敗：那個 body 會被記成「任何值」，並在啟動時以 `tracing` 印出警告，說明是哪條路由、哪個型別。在測試裡檢查 `warnings` 是空的，就能確保文件完整：

```rust
use hornero::openapi::{OpenApiConfig, warnings};
use hornero::prelude::*;
use serde::Deserialize;

#[derive(Deserialize)] // 忘了 ToSchema
struct Note {
    text: String,
}

#[controller("/notes")]
struct NoteController;

#[routes]
impl NoteController {
    #[post("/")]
    async fn create(&self, Json(note): Json<Note>) -> String {
        note.text
    }
}

#[module(controllers = [NoteController])]
struct AppModule;

let routes = Hornero::new(AppModule).routes().unwrap();
assert_eq!(
    warnings(&routes, &OpenApiConfig::new("Notes", "1.0")),
    ["POST /notes (NoteController::create): `Note` does not implement `utoipa::ToSchema`, \
      so it is documented as any value; add `#[derive(ToSchema)]` to it"]
);
```

## 不啟動 server 輸出文件

`hornero::openapi::document` 只需要路由表。`Hornero::routes()` 不會建構任何 provider、也不會連資料庫，所以可以在測試或 build 步驟裡把文件寫成檔案，交給前端產生 client：

```rust,no_run
use hornero::openapi::{OpenApiConfig, document};
use hornero::prelude::*;
# #[module]
# struct AppModule;

let routes = Hornero::new(AppModule).global_prefix("/api").routes().unwrap();
let doc = document(&routes, &OpenApiConfig::new("Todo API", "1.0"));
std::fs::write("openapi.json", doc.to_pretty_json().unwrap()).unwrap();
```

## `OpenApiConfig`

| 方法 | 作用 |
|---|---|
| `new(title, version)` | 文件標題與 API 版本 |
| `.description(text)` | API 說明，可以用 Markdown |
| `.path(path)` | Swagger UI 的位置，預設 `/docs`；文件在 `{path}/openapi.json` |
| `.server(url)` | API 的網址；沒有設定時，Swagger UI 對提供文件的主機發 request |
| `.tag(name, description)` | 描述一個 tag；宣告過的 tag 排在前面 |
| `.schema::<T>()` | 把沒有出現在任何 handler 簽名裡的型別放進 `components.schemas` |
| `.filter(fn(&RouteInfo) -> bool)` | 只記錄部分路由 |
| `.modify(fn(&mut OpenApi))` | 最後修改產生的文件，處理 hornero 推導不了的任何東西 |

每次呼叫 `OpenApiModule::for_root` 都是一個獨立的模組，所以可以用 `path` 與 `filter` 提供兩份文件，例如對外 API 與後台 API：

```rust
use hornero::openapi::{OpenApiConfig, OpenApiModule};
use hornero::prelude::*;
# #[module]
# struct ShopModule;
# #[module]
# struct AdminModule;

#[module(imports = [
    OpenApiModule::for_root(
        OpenApiConfig::new("Shop API", "1.0").filter(|route| route.module() != "AdminModule")
    ),
    OpenApiModule::for_root(
        OpenApiConfig::new("Admin API", "1.0")
            .path("/admin/docs")
            .filter(|route| route.module() == "AdminModule")
    ),
    ShopModule,
    AdminModule,
])]
struct AppModule;
# Hornero::new(AppModule).validate().unwrap();
```

文件本身是公開的：`OpenApiModule` 掛上的是一般的 axum router，不經過任何 guard。不想在正式環境公開文件時，手寫根模組的 `define`，依設定決定要不要 `m.import(OpenApiModule::for_root(...))`（寫法見 [動態模組](03-modules.md#動態模組)）。

## 對照 @nestjs/swagger

| @nestjs/swagger | hornero |
|---|---|
| `DocumentBuilder` | `OpenApiConfig` |
| `SwaggerModule.setup('docs', app, document)` | `OpenApiModule::for_root(config)` |
| `SwaggerModule.createDocument` | `hornero::openapi::document(&routes, &config)` |
| `@ApiProperty()` | `#[derive(ToSchema)]`，欄位上的 `#[schema(...)]` |
| Swagger CLI plugin（從 TypeScript 型別推導） | `#[routes]` 從 handler 簽名推導，預設開啟 |
| `@ApiOperation({ summary })` | doc comment |
| `@ApiResponse()` | `#[api_response(...)]` |
| `@ApiBody()` | `#[api_body(...)]` |
| `@ApiTags()` | `#[api_tags(...)]`，預設是 controller 名稱 |
| `@ApiBearerAuth()` | 在 guard 上 `impl ApiSecurity` |
| `@ApiExcludeEndpoint()`、`@ApiExcludeController()` | `#[api_exclude]` |

完整的 API 見 [`hornero-openapi` API 參考](../api/hornero-openapi.md)，設計取捨見 [ADR 0006](../adr/0006-openapi-inference.md)。

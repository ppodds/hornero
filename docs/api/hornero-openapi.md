# `hornero-openapi` API 參考

從路由表與 handler 簽名產生 OpenAPI 3.1 文件，並提供 Swagger UI，相當於 `@nestjs/swagger`。建立在 [utoipa](https://docs.rs/utoipa) 6 之上。應用程式透過 `hornero` 的 feature 使用它，路徑是 `hornero::openapi`：

```toml
[dependencies]
hornero = { path = "crates/hornero", features = ["swagger-ui"] }   # 或只開 "openapi"，不提供 Swagger UI
utoipa = "6"                                                 # derive macro 產生的程式碼使用 utoipa 路徑
```

| 項目 | 用途 |
|---|---|
| [`OpenApiModule`](#openapimodule) | 提供文件與 Swagger UI 的模組 |
| [`OpenApiConfig`](#openapiconfig) | 文件的標題、版本、位置與其他設定 |
| [`document`](#document-與-warnings) | 從路由表產生文件，不需要啟動 server |
| [`warnings`](#document-與-warnings) | 推導不出來的地方 |
| [`ApiSecurity`](#trait-apisecurity) | 讓 guard 在文件中宣告 security scheme |
| [`SecurityScheme`](#securityscheme) | security scheme 與常見 scheme 的捷徑 |
| [`describe_security`](#describe_security) | 手寫 `Injectable` 的 guard 用來記錄 scheme |
| `ToSchema`、`IntoParams` | re-export 的 utoipa derive 與 trait，給 DTO 與 query 型別使用 |
| `OpenApi` | re-export 的 `utoipa::openapi::OpenApi`，也就是產生的文件 |
| `utoipa` | re-export 的 utoipa crate |

handler 上的 `#[api_response]`、`#[api_body]`、`#[api_tags]`、`#[api_exclude]` 屬於 `hornero` facade，見 [`hornero` API 參考](hornero.md#openapi-attributeapi_responseapi_bodyapi_tagsapi_exclude)。推導規則與完整範例見 [OpenAPI](../guide/16-openapi.md)。

### Feature

| Feature（`hornero` 的） | 內容 |
|---|---|
| `openapi` | `hornero::openapi`；`#[routes]` 推導每個 handler 的文件、`#[derive(Injectable)]` 記錄 guard 的 security scheme；`OpenApiModule` 只提供 `{path}/openapi.json` |
| `swagger-ui` | `openapi`，加上 `{path}` 的 Swagger UI。UI 的檔案在編譯時嵌進執行檔 |

直接依賴 `hornero-openapi` 時，`swagger-ui` 是它的預設 feature。

---

## `OpenApiModule`

```rust,ignore
pub struct OpenApiModule { /* ... */ }

impl OpenApiModule {
    pub fn for_root(config: OpenApiConfig) -> Self;
}

impl Module for OpenApiModule { /* ... */ }
```

在根模組 import 它，相當於 Nest 的 `SwaggerModule.setup`。它在啟動時注入內建的 [`Arc<Routes>`](hornero-core.md#routesrouteinfo-與-enhancerinfo)，產生一次文件，並掛上這些路由（都在 `Hornero::global_prefix` 之下）：

| 路由 | 內容 |
|---|---|
| `GET {path}/openapi.json` | 文件，`application/json` |
| `GET {path}` | 303 轉址到 `{path}/`（`swagger-ui` feature） |
| `GET {path}/` 與 `{path}/*` | Swagger UI 與它的檔案（`swagger-ui` feature） |

| 方法 | 說明 |
|---|---|
| `fn for_root(config: OpenApiConfig) -> Self` | 提供 `config` 描述的文件。每次呼叫都是不同的模組（`Module::id` 各不相同），所以兩個 `OpenApiModule` 可以用不同的 `path` 與 `filter` 提供兩份文件 |

`config.path` 是 `/` 時，啟動會失敗（`BootError`）：文件需要自己的路徑。

---

## `OpenApiConfig`

```rust,ignore
#[derive(Clone)]
pub struct OpenApiConfig { /* ... */ }
```

文件整體的資訊與提供文件的位置，相當於 Nest 的 `DocumentBuilder`。

| 方法 | 說明 |
|---|---|
| `fn new(title: impl Into<String>, version: impl Into<String>) -> Self` | 標題與 API 版本；位置預設 `/docs` |
| `fn description(self, description: impl Into<String>) -> Self` | API 說明，可以用 Markdown |
| `fn path(self, path: impl Into<String>) -> Self` | Swagger UI 的位置；文件在 `{path}/openapi.json`。前後的 `/` 會被正規化，`"docs/"` 等於 `"/docs"` |
| `fn server(self, url: impl Into<String>) -> Self` | 加上一個 server，例如 `https://api.example.com`；可以呼叫多次 |
| `fn tag(self, name: impl Into<String>, description: impl Into<String>) -> Self` | 描述一個 tag。文件的 `tags` 先列出這裡宣告的 tag，再依路由第一次使用的順序列出其他 tag |
| `fn schema<T: ToSchema>(self) -> Self` | 把 `T` 與它引用的 schema 放進 `components.schemas`，給沒有出現在 handler 簽名裡的型別使用 |
| `fn filter(self, filter: impl Fn(&RouteInfo) -> bool + Send + Sync + 'static) -> Self` | 只記錄 `filter` 回傳 `true` 的路由 |
| `fn modify(self, modify: impl Fn(&mut OpenApi) + Send + Sync + 'static) -> Self` | 最後修改產生的文件；依加入的順序執行 |

```rust
use hornero::openapi::{OpenApiConfig, document};
use hornero::prelude::*;

#[controller("/health")]
struct HealthController;

#[routes]
impl HealthController {
    #[get("/")]
    async fn check(&self) -> &'static str {
        "ok"
    }
}

#[module(controllers = [HealthController])]
struct AppModule;

let config = OpenApiConfig::new("Status API", "2.1")
    .description("Liveness checks.")
    .server("https://status.example.com")
    .tag("Health", "Is it up?")
    .modify(|doc| doc.info.title.push_str(" (beta)"));
let doc = document(&Hornero::new(AppModule).routes().unwrap(), &config);
assert_eq!(doc.info.title, "Status API (beta)");
assert_eq!(doc.servers.unwrap()[0].url, "https://status.example.com");
assert_eq!(doc.tags.unwrap()[0].description.as_deref(), Some("Is it up?"));
```

---

## `document` 與 `warnings`

```rust,ignore
pub fn document(routes: &Routes, config: &OpenApiConfig) -> OpenApi;
pub fn warnings(routes: &Routes, config: &OpenApiConfig) -> Vec<String>;
```

| 函式 | 說明 |
|---|---|
| `document` | 產生 `routes` 的文件。`routes` 來自 [`Hornero::routes`](hornero-core.md#hornero)（啟動前，不建構任何東西）或 `App::routes`（啟動後）。推導不出來的地方以 `tracing` 的 warning 記錄，target 是 `hornero_openapi` |
| `warnings` | `document` 推導不出來的地方，每個問題一行，開頭是路由（`POST /notes (NoteController::create): ...`）。全部推導成功時是空的，適合在測試中檢查 |

會產生 warning 的情況：

- body 或回應的型別沒有實作 `ToSchema`：記成任何值。
- `Path<T>` 或 `Query<T>` 的 `T` 既沒有 `IntoParams` 也沒有 `ToSchema`：不記錄它的參數（路徑參數仍以字串記錄）。
- handler 取出的路徑參數數量與路徑不符。
- 兩個不同的 schema 使用同一個名稱：保留第一個，另一個用 utoipa 的 `#[schema(as = ...)]` 改名。
- 文件引用了不存在的 schema（例如 `IntoParams` 型別的欄位引用了別的型別）：用 `OpenApiConfig::schema` 補上。
- 自訂的 HTTP method：OpenAPI 無法描述，路由不列入。

產生的文件：

- `openapi` 是 `3.1.0`；路徑依路由表的順序排列，axum 的 `{*rest}` 寫成 `{rest}`。
- `operationId` 是 `{controller}_{handler}`；預設的 tag 是 controller 名稱去掉 `Controller`。
- 錯誤回應的 body 是 `components.schemas.ProblemDetails`，對應 `HttpError` 的 problem+json 格式，content type 是 `application/problem+json`。

---

## `trait ApiSecurity`

```rust,ignore
pub trait ApiSecurity {
    fn scheme() -> (&'static str, SecurityScheme);
    fn required(_route: &RouteInfo) -> bool { true }
    fn scopes(_route: &RouteInfo) -> Vec<String> { Vec::new() }
}
```

讓 guard 在文件中宣告 security scheme，相當於 Nest 的 `@ApiBearerAuth()`，但只需寫一次。guard 執行的每一條路由（`Hornero::guard`、`AppGuard`、controller 與 handler 的 `#[guards]`）都會要求這個 scheme；一條路由上有多個這樣的 guard 時，全部都要滿足。

| 方法 | 說明 |
|---|---|
| `scheme()` | scheme 在 `components.securitySchemes` 中的名稱與內容；名稱相同的 guard 共用一個 scheme |
| `required(route)` | 這條路由是否需要這個 scheme，預設都需要。讓它和 `can_activate` 的判斷一致，例如略過 `#[meta(Public)]` 的路由 |
| `scopes(route)` | 這條路由需要的 OAuth2 / OpenID Connect scope，預設沒有 |

開啟 `openapi` feature 時，`#[derive(Injectable)]` 與 `#[injectable]` 會記錄實作了 `ApiSecurity` 的型別的 scheme；手寫的 `Injectable` 用 [`describe_security`](#describe_security)。

---

## `SecurityScheme`

```rust,ignore
#[derive(Clone, PartialEq)]
pub struct SecurityScheme(/* utoipa::openapi::security::SecurityScheme */);

impl From<utoipa::openapi::security::SecurityScheme> for SecurityScheme;
```

| 方法 | 說明 |
|---|---|
| `fn bearer() -> Self` | `Authorization: Bearer <token>` |
| `fn bearer_jwt() -> Self` | 同上，token 格式是 JWT |
| `fn basic() -> Self` | `Authorization: Basic <credentials>` |
| `fn api_key_header(name: impl Into<String>) -> Self` | 放在 header `name` 的 API key |
| `fn api_key_query(name: impl Into<String>) -> Self` | 放在 query 參數 `name` 的 API key |
| `fn api_key_cookie(name: impl Into<String>) -> Self` | 放在 cookie `name` 的 API key |
| `fn bearer_format(self, format: impl Into<String>) -> Self` | bearer token 格式的提示；對其他 scheme 沒有作用 |
| `fn description(self, description: impl Into<String>) -> Self` | scheme 的說明，Swagger UI 的 Authorize 視窗會顯示 |
| `fn into_inner(self) -> utoipa::openapi::security::SecurityScheme` | 取出 utoipa 的 scheme |

OAuth2 與 OpenID Connect 等其他 scheme，用 utoipa 的型別建構後 `.into()`。

---

## `describe_security`

```rust,ignore
pub fn describe_security<G: ApiSecurity>(meta: &mut Metadata);
```

在 [`Injectable::describe`](hornero-core.md#trait-injectable) 中記錄 `G` 的 security scheme，給手寫 `Injectable` 的 guard 使用。用 derive 的 guard 不需要它。

```rust
use hornero::openapi::{ApiSecurity, OpenApiConfig, SecurityScheme, describe_security, document};
use hornero::prelude::*;
use hornero::{BoxError, Key, Metadata, Resolver};

struct ApiKeyGuard;

impl Injectable for ApiKeyGuard {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }

    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Ok(ApiKeyGuard)
    }

    fn describe(meta: &mut Metadata) {
        describe_security::<Self>(meta);
    }
}

impl Guard for ApiKeyGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        match ctx.parts.headers.get("x-api-key") {
            Some(_) => Ok(()),
            None => Err(HttpError::unauthorized("missing API key")),
        }
    }
}

impl ApiSecurity for ApiKeyGuard {
    fn scheme() -> (&'static str, SecurityScheme) {
        ("api_key", SecurityScheme::api_key_header("x-api-key"))
    }
}

#[controller("/reports")]
#[guards(ApiKeyGuard)]
struct ReportController;

#[routes]
impl ReportController {
    #[get("/")]
    async fn list(&self) -> &'static str {
        "[]"
    }
}

#[module(controllers = [ReportController], providers = [ApiKeyGuard])]
struct AppModule;

let doc = document(&Hornero::new(AppModule).routes().unwrap(), &OpenApiConfig::new("Reports", "1.0"));
let doc = serde_json::to_value(&doc).unwrap();
assert_eq!(doc["paths"]["/reports"]["get"]["security"], serde_json::json!([{ "api_key": [] }]));
assert_eq!(doc["components"]["securitySchemes"]["api_key"]["in"], "header");
```

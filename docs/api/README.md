# API 參考

每個 crate 一頁，列出所有公開的型別、trait、方法與欄位，附上簽名與可執行的範例。想依概念學習時，請讀 [指南](../guide/README.md)。

| Crate | 內容 | 什麼時候需要 |
|---|---|---|
| [`hornero`](hornero.md) | facade、`prelude`、所有 macro 的完整語法 | 一定需要；應用程式只依賴這個 crate |
| [`hornero-core`](hornero-core.md) | `Hornero`、`App`、`Module`、`ProviderDef`、`Injectable`、guard / interceptor / filter / middleware trait、`HttpError`、`BootError`、`Lifecycle`… | 透過 `hornero::` 使用，不需要直接依賴 |
| [`hornero-testing`](hornero-testing.md) | `TestApp`、`TestClient`、`TestRequest`、`TestResponse` | 測試（`[dev-dependencies]`） |
| [`hornero-config`](hornero-config.md) | `ConfigModule`、`Source` | 從 TOML 與環境變數讀取型別化的設定 |
| [`hornero-validation`](hornero-validation.md) | `Valid<E>`、`Validated` | 以 garde 驗證 request |
| [`hornero-sea-orm`](hornero-sea-orm.md) | `SeaOrmModule<Tag>`、`Db<Tag>`、`Primary` | 使用 SeaORM 存取資料庫 |
| [`hornero-openapi`](hornero-openapi.md) | `OpenApiModule`、`OpenApiConfig`、`document`、`ApiSecurity`、`SecurityScheme` | 產生 OpenAPI 文件與 Swagger UI（`hornero` 的 `openapi` / `swagger-ui` feature） |

## 依用途查詢

| 我想要… | 看這裡 |
|---|---|
| 啟動應用、設定全域前綴、加上 tower layer | [`Hornero`](hornero-core.md#hornero) |
| 宣告 provider | [`#[derive(Injectable)]`](hornero.md#deriveinjectable)、[`#[injectable]`](hornero.md#injectable) |
| 宣告模組、import / export | [`#[module]`](hornero.md#module)、[`ModuleDef`](hornero-core.md#moduledef) |
| 用 trait object 注入 | [`bind!`](hornero.md#bind) |
| 註冊值、factory 或自訂 provider | [`ProviderDef`](hornero-core.md#providerdef) |
| 寫 controller 與路由 | [`#[controller]`](hornero.md#controller)、[`#[routes]`](hornero.md#routes) |
| 驗證身分、檢查權限 | [`Guard`](hornero-core.md#trait-guard-與-guardcontext)、[`Metadata`](hornero-core.md#metadata) |
| 包住 handler（計時、快取、改寫回應） | [`Interceptor`](hornero-core.md#trait-interceptor) |
| 自訂錯誤格式 | [`ExceptionFilter`](hornero-core.md#trait-exceptionfilter)、[`HttpError`](hornero-core.md#httperror) |
| 需要依賴的 middleware | [`Middleware`](hornero-core.md#trait-middleware) |
| 每個 request 一個實例、讀取目前的 request | [`Scope`](hornero-core.md#enum-scope)、[`RequestContext`](hornero-core.md#requestcontext) |
| 啟動與關閉時執行程式碼 | [`Lifecycle`](hornero-core.md#trait-lifecycle)、[`Shutdown`](hornero-core.md#shutdown) |
| 處理啟動錯誤 | [`BootError`](hornero-core.md#booterror)、[`Problem`](hornero-core.md#enum-problem) |
| 列出所有路由 | [`Routes`](hornero-core.md#routesrouteinfo-與-enhancerinfo) |
| 產生 OpenAPI 文件、Swagger UI | [`OpenApiModule`](hornero-openapi.md#openapimodule)、[`#[api_response]`](hornero.md#openapi-attributeapi_responseapi_bodyapi_tagsapi_exclude) |
| 寫測試、替換 provider 或 guard | [`TestApp`](hornero-testing.md#testapp)、[`TestAppBuilder`](hornero-testing.md#testappbuilder) |

## rustdoc

每個公開項目在原始碼裡也有 rustdoc 說明，可以離線瀏覽與搜尋：

```sh
cargo doc --workspace --no-deps --open
```

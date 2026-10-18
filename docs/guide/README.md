# hornero 指南

這份指南從零開始介紹 hornero 的所有概念。前四章足以寫出完整的 API；之後的章節可以依需要閱讀。

| 章節 | 內容 |
|---|---|
| [1. 快速入門](01-getting-started.md) | 安裝、第一個應用、第一個測試，以及啟動時發生了什麼 |
| [2. Provider 與依賴注入](02-providers.md) | `#[derive(Injectable)]`、constructor injection、`value` / `factory` / `bind!`、trait object |
| [3. 模組](03-modules.md) | imports / exports、嚴格可見性、global 模組、動態模組 |
| [4. Controller 與路由](04-controllers.md) | `#[controller]`、`#[routes]`、extractor、回傳型別、路徑規則 |
| [5. Request pipeline](05-request-pipeline.md) | guard、interceptor、exception filter、middleware、tower layer 與它們的順序 |
| [6. Scope](06-scopes.md) | singleton / request / transient、`RequestContext`、scope 傳染與 scope report |
| [7. 生命週期與關閉](07-lifecycle.md) | `Lifecycle` hook、`Shutdown`、graceful shutdown、背景工作 |
| [8. 錯誤處理](08-errors.md) | `HttpError`、problem+json、domain error、`BootError` 診斷 |
| [9. 測試](09-testing.md) | `validate()`、`TestApp`、provider / guard override、斷言 |
| [10. 設定](10-configuration.md) | `hornero-config`：型別化設定、TOML 與環境變數 |
| [11. 驗證](11-validation.md) | `hornero-validation`：`Valid<Json<T>>` 與 422 錯誤格式 |
| [12. 資料庫](12-database.md) | `hornero-sea-orm`：`SeaOrmModule`、`Db<Tag>`、repository 模式 |
| [13. 不用 macro](13-without-macros.md) | L0–L3 漸進式採用、手寫 `Module` / `Injectable` / `Controller`、自製整合 crate |
| [14. 常見做法](14-recipes.md) | CORS、request id、背景工作、多資料庫、嵌入既有 axum 應用… |
| [15. 從 NestJS 過來](15-from-nestjs.md) | 概念對照表與 FAQ |
| [16. OpenAPI](16-openapi.md) | `hornero-openapi`：從 handler 簽名推導文件、Swagger UI、從 guard 推導身分驗證 |

## 閱讀前的約定

- 範例都使用 `use hornero::prelude::*;`，它包含 `Arc`、`Json`、`Path`、`Query`、`StatusCode`、`IntoResponse`、`Response`，以及 hornero 最常用的型別與 macro。
- 程式碼中以 `# ` 開頭的行是 doctest 用的隱藏行（通常是 `async fn main` 的包裝），閱讀時可以忽略。
- 需要 `#[tokio::test]` 的範例取自 `docs/tests` 裡真正會執行的測試檔。
- hornero 是實驗性專案，尚未發佈到 crates.io，API 可能隨時變動。

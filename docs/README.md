# hornero 文件

hornero 是建立在 tokio 與 axum 0.8 之上、NestJS 風格的 Rust Web framework：模組、依賴注入、controller、guard、interceptor、exception filter，而且不把 axum 藏起來。

## 從哪裡開始

| 你想要… | 讀這裡 |
|---|---|
| 十分鐘做出第一個 API | [快速入門](guide/01-getting-started.md) |
| 有系統地學完所有概念 | [指南](guide/README.md)，依章節順序閱讀 |
| 查某個型別或方法怎麼用 | [API 參考](api/README.md) |
| 從 NestJS 轉過來 | [從 NestJS 過來](guide/15-from-nestjs.md) |
| 解決常見需求（CORS、背景工作、多資料庫…） | [常見做法](guide/14-recipes.md) |
| 了解設計取捨 | [設計說明](design.md) 與 [ADR](adr/0001-runtime-di.md) |
| 看一個完整的應用 | [`examples/todo`](../examples/todo/src/lib.rs) |

## 指南目錄

1. [快速入門](guide/01-getting-started.md)
2. [Provider 與依賴注入](guide/02-providers.md)
3. [模組](guide/03-modules.md)
4. [Controller 與路由](guide/04-controllers.md)
5. [Request pipeline：guard、interceptor、filter、middleware](guide/05-request-pipeline.md)
6. [Scope：singleton、request、transient](guide/06-scopes.md)
7. [生命週期與關閉](guide/07-lifecycle.md)
8. [錯誤處理](guide/08-errors.md)
9. [測試](guide/09-testing.md)
10. [設定：hornero-config](guide/10-configuration.md)
11. [驗證：hornero-validation](guide/11-validation.md)
12. [資料庫：hornero-sea-orm](guide/12-database.md)
13. [不用 macro：漸進式採用與自製整合 crate](guide/13-without-macros.md)
14. [常見做法](guide/14-recipes.md)
15. [從 NestJS 過來](guide/15-from-nestjs.md)
16. [OpenAPI：hornero-openapi](guide/16-openapi.md)

## API 參考

- [`hornero`](api/hornero.md)：facade、`prelude`，以及所有 macro 的完整語法
- [`hornero-core`](api/hornero-core.md)：`Hornero`、`App`、`Module`、`ProviderDef`、guard / interceptor / filter / middleware trait、`HttpError`、`BootError`…
- [`hornero-testing`](api/hornero-testing.md)：`TestApp`、`TestClient`
- [`hornero-config`](api/hornero-config.md)：`ConfigModule`、`Source`
- [`hornero-validation`](api/hornero-validation.md)：`Valid<E>`
- [`hornero-sea-orm`](api/hornero-sea-orm.md)：`SeaOrmModule`、`Db<Tag>`
- [`hornero-openapi`](api/hornero-openapi.md)：`OpenApiModule`、`OpenApiConfig`、`ApiSecurity`

rustdoc 版本的 API 文件（含原始碼連結與搜尋）：

```sh
cargo doc --workspace --no-deps --open
```

## 這些文件是測試過的

指南、設計說明、ADR 與根目錄 `README.md` 裡的每一段 Rust 程式碼，只有三種處理方式，沒有第四種：

- 一般的 Rust 程式碼區塊是 doctest，會被編譯，大多數也會實際執行。開頭為 `# ` 的行是 doctest 用的隱藏行，在 rustdoc 中不會顯示；會啟動 server 或需要資料庫的區塊標 `no_run`（只編譯不執行），示範「不該通過編譯」的區塊標 `compile_fail`。
- 標了 `<!-- snippet: ... -->` 的區塊取自 `docs/examples` 或 `docs/tests` 裡真正的檔案（例如需要 `#[tokio::test]` 的測試範例），測試會確認兩邊內容一致。
- 只有真正的片段（trait 簽章摘要、需要外部 crate 的範例、節錄自 `examples/todo` 的程式碼）可以標 `rust,ignore`，而且必須在前一行寫 `<!-- ignore: 原因 -->`；沒有原因的 `ignore` 區塊會讓 `docs/tests/markdown.rs` 失敗。

例外是 [API 參考](api/README.md)：那裡的 `rust,ignore` 區塊是單純的簽章（例如 `pub struct Hornero { /* ... */ }`），不需要標記，也不會被編譯。API 參考必須提到每一個公開的型別、方法與欄位，少了任何一個測試就會失敗。

```sh
cargo test -p hornero-docs                    # 執行所有文件測試
cargo doc -p hornero-docs --no-deps --open    # 在瀏覽器中閱讀整份指南
```

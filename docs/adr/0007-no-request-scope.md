# ADR-0007：移除 request scope，request 資料走 extractor

- 狀態：已採納，取代 [ADR-0002](0002-scope-bubbling.md)
- 日期：2026-11-06

## 背景

ADR-0002 在三個選項裡選了 Nest 式傳染：(a) 只有 singleton，request 資料一律走 extractor；(b) 明確 request scope、不傳染；(c) Nest 式傳染。理由是保有「深層 service 不必層層傳參數就能拿到目前使用者」的體驗。

實作之後，這個體驗的代價比預期高，帶來的好處卻很少被用到：

- **框架內最複雜的一段程式碼只為了它存在。** request 在 pipeline 裡會被 guard 與 interceptor 修改，又會被 handler 消耗，所以 `RequestContext` 要在每個階段拍快照、作廢、替 filter 保留，還要避開 `Arc` 循環。每條路由也因此要帶 `ScopeExt`，每個元件都要透過 `Handle` 區分 singleton 與 request-scoped。
- **錯誤延到執行期。** request-scoped 元件在 request 期間建構，建構失敗變成 500。這違反 fail fast：其他依賴問題都在 bind port 之前回報。
- **傳染讓遠處的改動改變整條鏈。** 一個深層依賴改成 request scope，controller 就開始每個 request 重建。scope report 能讓它被看見，但這是在替問題加告示牌，而不是解決問題。
- **它與 hornero 其他選擇不一致。** [design.md](../design.md) 已經把 CLS 隱式交易換成明確傳遞；隱式取得目前使用者是同一件事。
- **沒有實際使用者。** `examples/todo` 已經改成 singleton `AuditLog`，使用者由 handler 的 `AuthUser` 參數傳入。指南自己也建議優先用 extractor。

axum 的 extractor 正是為這件事設計的，而且 hornero 的設計原則就是不包裝 axum（ADR-0004）。

## 決策

- `Scope::{Singleton, Transient}`。**所有 provider 都在啟動時建構，request 期間不碰容器。** handler 與 enhancer 直接持有 `Arc`。
- 移除 `Scope::Request`、`RequestContext`、`ScopeReport`、`ScopeEntry`、`App::scope_report`、`ProviderDef::pinned`、`Injectable::SCOPE_PINNED`。`Hornero::validate` 回傳 `Result<(), BootError>`。
- request 資料：guard、interceptor 與 middleware 寫進 extensions，handler 用 extractor 讀取，再用參數傳給 service。
- exception filter 在 request 被消耗之後才執行，extractor 到不了那裡。filter 因此只看得到錯誤與路由，不替它保留 request 快照：那需要在成功路徑上為錯誤路徑付出複製，也正是被移除的那段機制。依 request 呈現錯誤改由手上還有 request 的 interceptor 或 middleware 負責，用新增的 `HttpError::in_response` 取得錯誤。
- transient controller 是 `ScopeViolation`。它過去會在每個 request 重建，那是 `Handle` 的副作用，文件沒有承諾過；靜默地改成只建構一次會改變行為，所以直接報錯。

## 後果

- 「所有 provider 在 bind port 之前建構完成」沒有例外，失敗一定是 `BootError`。
- 路由不再需要 scope layer，request 路徑上只剩 `Arc` clone。
- **代價：深層 service 要用參數取得 request 資料。** 呼叫鏈很深時會比較囉嗦，可以包成 context struct 傳遞；換來的是每一層的依賴都寫在簽名上。
- **extractor 拿不到 provider**（handler 的 state 是 `()`）。需要 DI 的工作要放在 guard、interceptor 或 middleware，由它們把結果寫進 extensions。
- 依 request 呈現錯誤不再是 filter 的工作：interceptor 看不到 guard 的拒絕，middleware 看不到 guard 寫入的資料，兩者要依需求選擇。
- 來自 Nest 的使用者少了 `Scope.REQUEST` 與 `@Inject(REQUEST)` 的直接對應，[對照表](../guide/15-from-nestjs.md) 指向 extractor。

# ADR-0002：Provider scope 與 Nest 式傳染

- 狀態：已採納
- 日期：2026-10-17

## 背景

Nest 提供 `DEFAULT`（singleton）、`REQUEST`、`TRANSIENT` 三種 scope，且 request scope 會沿依賴鏈往上「傳染」：依賴 request-scoped provider 的元件自己也變成 request-scoped（包括 controller）。這很方便，但也是隱性效能懸崖：一個深層依賴改成 request scope，可能讓整條鏈在每個 request 重建。

討論時考慮過：(a) 只有 singleton，request 資料一律走 extractor；(b) 明確 request scope、不傳染（違反即報錯）；(c) Nest 式傳染。最後選擇 (c)。

## 決策

- `Scope::{Singleton, Request, Transient}`。
- **Effective scope**：自己宣告為 Request，或任一依賴的 effective scope 為 Request → Request；否則維持宣告值。規則同樣適用於 controller、guard、interceptor、filter。
- **Transient**：每個注入點建立新實例，不快取。
- 為了讓傳染「看得見」並避免誤用，加入以下控管：
  - **Scope report**：啟動時輸出每個 request-scoped 元件及完整傳染鏈；也可由 `Hornero::validate()` / `App::scope_report()` 取得並在測試中斷言。
  - **啟動期錯誤**：
    - middleware 與 mount 變成 request-scoped（它們在啟動時就要實例化）；
    - 有 lifecycle hooks 的 provider 不是 singleton。Nest 對此是靜默不呼叫 hook，hornero 改為報錯；
    - 明確宣告 scope 的 provider（`#[injectable(scope = singleton)]`、`ProviderDef::pinned()`）被傳染。這是讓使用者自選的防護。
- `RequestContext`（相當於 Nest 的 `REQUEST`）是內建的 request-scoped provider。

## Runtime 設計

- 只有當 route 鏈上確實有 request-scoped 元件時才加上 scope layer；純 singleton 路由零成本。
- `RequestScope` 使用啟動時算好的 slot index（`Box<[OnceCell<Instance>]>`），不在 request 期間做 `TypeId` 查表。
- `RequestContext` 是各元件**建構當下**的 request 快照：在每個 guard 與 interceptor 之後作廢，下一個需要它的元件重新拍一份，已建構的元件保留自己拿到的那份；因此 guard 寫入 extensions 的資料對之後建構的元件（包含 handler 端的 controller）可見。快照會移除 scope 本身以避免 `Arc` 循環。
- Request-scoped 元件建構失敗會變成 500 `HttpError`（細節只寫 log），並經過 exception filter。

## 後果

- 保有 Nest 的使用體驗：在深層 service 取得目前使用者，不必層層傳參數。
- 效能懸崖仍然存在，但會出現在啟動 log 與可斷言的報告中，而不是靜默發生。
- 範例 `examples/todo` 刻意讓 `TodoService` 依賴 request-scoped 的 `AuditLog` 以示範傳染鏈；實務上若不希望 controller 每次重建，可以把 request 資料改由 extractor 承載。

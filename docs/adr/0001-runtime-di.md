# ADR-0001：執行期 DI 容器 + 啟動期整圖驗證

- 狀態：已採納
- 日期：2026-10-17

## 背景

NestJS 的 DI 依賴 `reflect-metadata` 在執行期讀取建構子參數型別，Rust 沒有對應機制。可行方案有三：

1. **執行期 TypeId 容器**：以 `TypeId` 當 key，啟動時驗證並建構整張圖。
2. **型別層級編譯期 DI**：以 trait bound / type-level list 在編譯期證明每個依賴都被提供（shaku 一類）。
3. **codegen**：以 build step 分析程式碼並產生組裝程式（pavex 一類）。

## 決策

採用方案 1，並以以下手段補足「不是編譯期」的缺口：

- **整圖驗證在 bind port 之前完成**，且收集所有問題後一次回報（缺依賴、循環、不可見、歧義、scope 違規、路由衝突）。
- **`Hornero::validate()` 不實例化任何 provider**，可放在 `#[test]` 中，CI 幾毫秒內擋下接線錯誤。
- **啟動後沒有任何執行期查表**：依賴邊在啟動時解析為具體 node；handler 直接持有 singleton 的 `Arc`。不存在 actix `Data<T>` 那種「request 時才 500」的失敗模式。
- **其餘一切維持編譯期型別**：provider 欄位、handler 簽名、route metadata、config 都是具體型別。

## 理由

- proc macro 一次只看得到一個 item。跨模組、跨 crate 的 import/export 只能交給 trait 解析，代價是：
  - 缺依賴時是 HList / `Selector` 的 trait bound 錯誤；
  - 同一型別多個來源時因 overlap 只剩「type annotations needed」；
  - 循環依賴變成「overflow evaluating requirement」。
- 方案 2 無法依設定動態組裝模組，也很難在測試中 override provider。
- 大型應用整體成為一個巨大型別，trait solver 負擔重、增量編譯慢、rust-analyzer 體驗差。
- 生態圈先例：Rocket 的 Sentinel 同樣在 launch 時驗證；pavex 為了編譯期檢查選擇 codegen 而非純 trait，代價是額外工具鏈。

## 後果

- 錯誤在 `cargo test` 或啟動時發現，而不是 `cargo build`。以 `validate()` 測試與高品質錯誤訊息（insta snapshot 鎖定）彌補。
- 容器內部使用 `Arc<dyn Any>` 與 downcast；因為 key 與 value 型別由同一泛型函式寫入，downcast 不會失敗。

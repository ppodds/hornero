# ADR-0005：以 marker 型別作為 qualifier，並支援可選依賴

- 狀態：已採納
- 日期：2026-10-18

## 背景

hornero 以 `TypeId` 識別 provider（[ADR-0001](0001-runtime-di.md)），所以同一個模組無法註冊兩個同型別的 provider。原本的做法是 newtype（`struct PrimaryDb(Pool)`）或泛型 tag（`Db<Primary>`），兩者都要求**提供方**事先為每個實例設計一個型別。直接注入第三方型別（例如兩個 `reqwest::Client`）時，每個實例都得包一層 newtype。

Nest 用 `@Inject('TOKEN')` 解決同一個問題，並以 `@Optional()` 表示可以不存在的依賴。兩者都是「消費端如何指定依賴」的問題：hornero 的依賴宣告入口（derive 的 `Arc<T>` 欄位、建構函式參數、factory 參數、`Resolver::get`）都只能從型別推出 key，所以必須一起設計。

討論時考慮過的 qualifier 表示法：

1. **字串**（`#[inject("primary")]`）：最接近 Nest。打錯字要到 bootstrap 才發現，且字串沒有命名空間，跨 crate 容易撞名。
2. **marker 型別**（`#[inject(qualifier = Primary)]`）：打錯字是編譯錯誤，IDE 可以跳轉，型別路徑天然有命名空間。代價是每個 qualifier 要宣告一個空 struct。
3. **維持 newtype／泛型 tag**：不改框架，但沒有解決第三方型別的問題。

## 決策

採用方案 2。

- `Key` 由「型別 + 可選的 qualifier 型別」組成，兩者都以 `TypeId` 比較。`Pool`、`Pool@Primary`、`Pool@Replica` 是三個不同的 provider，彼此不會歧義，也不會互相替代。
- 提供方：`ProviderDef::qualified::<Q>()`；macro 的 `value(..)`、`factory(..)`、`bind!(..)`、`provider(..)` 後面可以接 builder 方法；class provider 寫成 `Qualified<T, Q>`。export 用 `ModuleDef::export_qualified::<T, Q>()` 或 `exports = [Qualified<T, Q>]`。
- 消費端：欄位與建構函式參數維持 `Arc<T>`，加上 `#[inject(qualifier = Q)]`；factory 參數無法加屬性，改用包裝型別 `Qualified<T, Q>`（deref 成 `T`）。
- 可選依賴：`Option<Arc<T>>`（或 `Option<Qualified<T, Q>>`）。`Key::optional()` 只描述依賴怎麼被要求，不參與比較。看不到任何 provider 時注入 `None`；**歧義仍是錯誤**，可見性規則不變。
- 新增 `Dependency` trait（`Arc<T>`、`Qualified<T, Q>`、`Option<D>`），factory 參數與 `Resolver::resolve::<D>()` 都透過它取得 key 與值，讓 key 與取值邏輯出自同一個型別。
- 不支援 `forwardRef`；循環依賴仍是 bootstrap 錯誤（[設計規則](../guide/02-providers.md#設計規則)）。

## 理由

- 維持「Types as tokens」：qualifier 也是型別，錯誤在編譯期或啟動期出現，而不是 request 期間。
- 依賴邊仍在啟動時解析成具體 node，可選依賴是否注入也在啟動時決定，執行期成本不變。
- 欄位型別保持 `Arc<T>`，使用方不必為了選擇實例而改變欄位型別；`Qualified<T, Q>` 只出現在無法加屬性的地方（closure 參數）與型別位置（`providers`、`exports`）。

## 後果

- 錯誤訊息以 `Pool@Primary` 表示帶 qualifier 的 provider；找不到時，hint 會列出同一型別已有的 qualifier。
- 可選依賴可能因為可見性（provider 沒有 export）而默默變成 `None`。啟動時會以 debug 等級記錄每個沒有注入的可選依賴。
- `FactoryFn` 的參數從 `Arc<T>` 一般化成任何 `Dependency`；`FactoryFn<Args>` 的 `Args` 型別參數改為參數型別本身的 tuple。
- newtype 與泛型 tag 仍然適用：實例的型別真的不同，或整個模組都要帶 tag（`SeaOrmModule<Tag>`）時，它們比 qualifier 清楚。

# ADR-0003：嚴格的模組可見性

- 狀態：已採納
- 日期：2026-10-17

## 背景

模組化是 Nest 的核心價值：provider 預設只在自己的模組可見，必須 export 才能被 import 方使用。另一種做法是扁平的全域容器（Spring 式），模組只是把註冊分組。

## 決策

採用嚴格可見性。從模組 `M` 解析型別 `T` 的順序：

1. `M` 自己的 provider（local）；
2. `M` import 的模組所 export 的 provider（包含 re-export：`export::<T>()` 匯出一個 import 來的 provider，或 `export_module::<N>()` 匯出整個 import 來的模組）；
3. global 模組（`m.global()`）export 的 provider。

同一層出現多個不同來源時是 **歧義錯誤**，不會靜默選一個；local 會遮蔽 import 來的同型別 provider。export 一個既非自己提供、也不是 import 而來的型別是啟動錯誤。

同一型別可以由不同模組各自私有提供（與 Nest 一致）；同一模組內重複註冊同一型別是錯誤，需要多實例時使用 qualifier（見 [ADR-0005](0005-qualifiers.md)）、newtype 或泛型 tag。

## 理由

- 大型專案的邊界清楚：哪些是模組的公開 API 一目了然。
- 可見性檢查只在啟動時進行；解析結果是「每條依賴邊指向具體 node」，執行期完全沒有可見性成本。
- 錯誤訊息可以給出精確提示：哪個模組提供／export 了缺少的型別，應該 import 或 export 什麼。

## 後果

- 比扁平容器多寫一些 `exports`。對 Config、資料庫連線這類橫切關注點，整合 crate 的模組預設為 global（可用 `.local()` 關閉）。
- Enhancer（guard 等）依同樣規則解析；不可見時在 controller 所屬模組建立私有的隱式實例，與 Nest 對 `@UseGuards(X)` 的行為一致。

# ADR-0004：不包裝 axum

- 狀態：已採納
- 日期：2026-10-17

## 背景

NestJS 風格的框架很容易長出自己的 `Request` / `Response` / `Context` 型別，把底層 HTTP 框架完全包住。這會讓 tower / tower-http / axum 生態系無法直接使用，也讓使用者必須重新學習一套 API。

## 決策

- facade crate 直接 re-export `axum` 與 `tower`。
- Handler 參數就是 axum extractor，回傳值就是 `IntoResponse`；guard 看到的是 `http::request::Parts`，interceptor 與 middleware 用的是 axum 的 `Request` 與 `Next`。
- 不需要 DI 的 middleware 直接用 tower layer（`Hornero::layer`、`ModuleDef::layer`），例如 `TraceLayer`、`CorsLayer`。
- 所有 enhancer 最終都編譯成 `axum::middleware::from_fn` 或 tower layer；hornero 不引入另一套執行模型。
- 任何既有的 axum `Router` 都能掛進模組（`mount_router`、可注入依賴的 `mount`），作為 L0 遷移路徑。
- Controller 不使用 axum state（`Router<()>`）：依賴在啟動時注入 controller，handler 閉包持有 controller 的 `Handle`。

## 後果

- 使用者可以逐步採用 hornero，也可以隨時退回純 axum。
- hornero 受 axum 的限制約束，例如 `MethodFilter` 不支援自訂 HTTP method、路徑參數語法是 `{param}`；hornero 在啟動時把這些情況轉成清楚的 `BootError`，而不是讓 axum panic。

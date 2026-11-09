# ADR-0008：路由 pipeline 由 hornero 執行，錯誤以值傳遞

- 狀態：已採納，部分取代 [ADR-0004](0004-no-axum-wrapping.md)
- 日期：2026-11-08

## 背景

ADR-0004 讓所有 enhancer 都編譯成 `axum::middleware::from_fn`，interceptor 直接用 axum 的 `Next`。這讓路由上的每一層只能交換 `Response`，錯誤一離開 handler 就變成回應，之後每一層都要從回應裡把它找回來：

- **exception filter 是重新渲染。** 錯誤附在回應的 extensions 上，filter 包在最外層找到它，再產生另一個回應。filter 同時負責「這是哪個狀態碼」與「回應長什麼樣子」，所以每個 domain error filter 都要重複應用的格式，錯誤攜帶的 header（例如 `WWW-Authenticate`）也要自己複製。
- **interceptor 看到的是渲染好的回應。** 它在 filter 內層，拿到的是轉換前的 500，與 client 收到的 404 不同；要知道原本的錯誤得用 `HttpError::in_response` 翻回應。只想處理成功回應的 interceptor 也會碰到錯誤回應。
- **axum extractor 的 rejection 沒有被整合。** 壞掉的 JSON、無法解析的路徑參數是 axum 的純文字回應，不是 `HttpError`，filter 看不到、格式也不一致。在 response 層補救（把純文字回應拆開、重組成 `HttpError`）是 workaround：每一層 layer 都要猜回應是不是錯誤。
- **`?` 的意義不清楚。** 任何錯誤經 `?` 都是 500，`catches` 用 `err.is::<T>()` 判斷要不要接，回傳 `bool` 的 `catches` 與回傳 `Response` 的 `catch` 之間沒有型別連結，常見的寫法是在 `catch` 裡再 `downcast_ref` 一次，`None` 分支只能猜。

要讓錯誤在路由裡保持為值，pipeline 的每一層就要交換 `Result<Response, HttpError>`，而 axum 的 `from_fn` 只能交換 `Response`。

## 決策

- **路由內的 pipeline 由 hornero 執行。** 每條路由是一個 axum handler（`on(MethodFilter, ..)`），裡面的 `RouteChain` 依序執行 guard、interceptor、extractor 與 handler，每一層都回傳 `Result<_, HttpError>`。路由比對、405、tower layer 與 middleware 仍是 axum 的。取代 ADR-0004 的兩點：interceptor 不再用 axum 的 `Next`，以及「所有 enhancer 都編譯成 `from_fn`，沒有另一套執行模型」。
- **interceptor 收到錯誤本身。** `Interceptor::intercept(&self, &RouteContext, Request, CallHandler) -> Result<Response, HttpError>`，`CallHandler::handle(req)` 執行內層。名稱沿用 Nest 的 `CallHandler`，也避開 middleware 仍在使用的 axum `Next`。
- **`HttpError` 分成回答與拋出。** 用狀態碼建構的（`not_found`、`new`、`from_boxed`）是回答，原樣渲染；`?`（`From<E: Error>`）轉換的是拋出的 domain error，記下 `type_name::<E>()`，原始錯誤保留為 source。
- **exception filter 依型別轉換。** `ExceptionFilter` 宣告 `type Error`，`catch(&self, &Self::Error, &RouteContext) -> HttpError`，hornero 以 `downcast_ref` 比對。只有拋出的錯誤會交給 filter，順序是 handler、controller、global，同一層依註冊順序，第一個相符的處理。錯誤離開 handler、extractor 或 interceptor 時就轉換，所以外層 interceptor 看到的是 client 會收到的狀態碼；filter 的結果一律以原始錯誤為 source（取代結果自己設的 source），`err.is::<TodoError>()` 照樣成立，而且結果不會再被轉換。拋出的錯誤外面加的 header（tuple、`with_header`）併到結果上，同名時以外面的為準；狀態碼由 filter 決定。
- **內建 catch-all。** 沒有 filter 相符的拋出錯誤是 500 `internal server error`，log 寫出型別與「add an ExceptionFilter for it to choose the status」。不在啟動時檢查每條路由的 filter 覆蓋：domain error 的型別在 `?` 之後才知道，要檢查就得讓每個 handler 宣告它可能拋出的型別，這個宣告成本比 catch-all 的風險高。
- **錯誤只渲染一次。** `Hornero::error_renderer` 設定應用的格式 `ErrorRenderer = fn(&HttpError, &RouteContext) -> Response`，預設 problem+json。hornero 接著加上錯誤攜帶的 header，並把錯誤放進回應的 extensions，給 middleware（`HttpError::in_response`）與 5xx log 使用。filter 只決定狀態碼與訊息。
- **axum 的 rejection 在參數處轉成錯誤。** handler 參數的 rejection 先轉成回應，再讀回來：帶有 `HttpError` 的（`Rejection = HttpError`、`Valid`）取回原值；其他 4xx / 5xx 取狀態碼、body 文字（上限 64 KB，有 `Content-Encoding` 的 body 不讀）與 header（不含描述舊 body 的 `Content-Type`、`Content-Length`、`Content-Encoding`）；非錯誤狀態（例如 `Redirect`）原樣送出。axum 預設 feature 裡的 rejection 型別（`QueryRejection` 這類組合型別，以及組成它們的 `FailedToDeserializeQueryString` 等變體；`ws`、`multipart` 等選用 feature 的型別 hornero 看不到，交給 filter）經 `?` 轉換時保留它們的 `status()` 與 `body_text()`，而且是回答，所以 `Extractor` 裡的 `Query::from_request_parts(..).await?` 仍是 400，filter 也和 handler 參數的 rejection 一樣看不到它。

## 後果

- 錯誤從拋出到渲染都是同一個值：filter、interceptor、renderer、middleware 與 log 看到的是同一個 `HttpError`，原始的 domain error 一路保留為 source。
- 應用的錯誤格式只寫一次。guard 的 401、domain error、axum 的 rejection 都由同一個 renderer 渲染，header 不必手動複製。
- interceptor 要處理兩條路：用 `?` 的 interceptor 只處理成功回應，對回應做的修改不會出現在錯誤上；要兩邊都處理就 match 結果，錯誤用 `with_header` 帶上 header。
- **handler 的回傳值仍會先渲染一次。** `Res: IntoResponse`，hornero 從回應的 extensions 把 `HttpError` 取回（`Arc::try_unwrap`）。要省掉這次渲染，`HttpError` 就不能是 `IntoResponse`（否則 `Result<T, HttpError>` 的兩種 impl 會有 E0283 歧義），代價是自訂 `IntoResponse` 的錯誤與 axum 慣用法都要改寫。這裡選擇相容。
- **handler 參數的 rejection 失去型別。** 讀回來的是狀態碼與文字，filter 無法以 `JsonRejection` 這類型別轉換它們。要保留型別需要 hornero 自己的 `Json`、`Query` 包裝，留待之後決定。
- renderer 只負責路由上的錯誤：404 fallback、405、panic、middleware 回傳的錯誤與關機逾時的 503 不經過路由，維持 problem+json。
- 破壞性變更：`Interceptor` 的簽章（`Next` → `CallHandler`、回傳 `Result`）、`ExceptionFilter`（新增 `type Error`、`catch` 回傳 `HttpError`、移除 `catches`）、`From<E>` 的意義（拋出，交給 filter），以及隱藏的 `RouteHandler` 方法。
- 只要使用 `CallHandler` 的 interceptor 與 `Extracted<E>` 都是 hornero 的概念，「隨時退回純 axum」對路由內部不再成立；handler 參數、`IntoResponse`、tower layer 與 mount 的 axum router 仍是原生的。

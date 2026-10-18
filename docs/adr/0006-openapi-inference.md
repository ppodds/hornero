# ADR-0006：OpenAPI 文件從路由表與 handler 簽名推導

- 狀態：已採納
- 日期：2026-10-18

## 背景

`@nestjs/swagger` 從 decorator 的 metadata 收集路由，再以 CLI plugin 讀 TypeScript 型別，幾乎不用手寫就能產生文件。Rust 生態系的 [utoipa](https://docs.rs/utoipa) 能從型別產生 schema（`#[derive(ToSchema)]`），但每個 handler 都要再寫一次 `#[utoipa::path(get, path = "/todos/{id}", params(..), responses(..))]`，而且路徑必須和路由重複一份，改了一邊忘了另一邊就不一致。

hornero 已經知道兩件 utoipa 不知道的事：完整的路由表（global prefix、controller prefix、handler 路徑，以及每條路由會經過哪些 guard），以及 `#[routes]` 看得到的 handler 簽名。

討論時考慮過的做法：

1. **包裝 `#[utoipa::path]`**：讓使用者照 utoipa 的方式寫，hornero 只負責收集。最少新程式碼，但路徑與參數仍要重複寫。
2. **只從路由表產生骨架**：路徑與 method 正確，參數與 body 全部靠屬性補。
3. **從路由表與 handler 簽名推導，屬性只補充**：`Path`、`Query`、`Json`、`Valid` 參數與 `Json`、`Result` 回傳型別都看得出來，只有狀態碼、錯誤回應這類型別表達不了的資訊才需要 `#[api_response]`。

## 決策

採用方案 3。

- **hornero-core 不依賴 utoipa**。它提供不建構任何東西的路由表 `Routes`（`Hornero::routes()`、`App::routes()`、可注入的 `Arc<Routes>`），以及讓 enhancer 描述自己的 `Injectable::describe` hook，結果放在每條路由的 `EnhancerInfo::meta`。
- **推導在 macro 裡以語法完成，型別能力以 autoref specialization 判斷**。`#[routes]` 只看得到語法，看不出 `Todo` 是否實作 `ToSchema`。產生的程式碼對 `&&Probe<Todo>` 呼叫方法，method resolution 會選實作了 `ToSchema` 的版本，否則選只記錄 warning 的 fallback。guard 的 `ApiSecurity` 也是同樣的做法，由 `#[derive(Injectable)]` 產生的 `describe` 檢查。
- **缺少 impl 不是編譯錯誤**，而是文件中的「任何值」加上啟動時的 warning；`hornero::openapi::warnings` 讓測試可以要求文件完整。
- **security 從 guard 推導**：實作 `ApiSecurity` 的 guard 執行到的每一條路由都要求它的 scheme，`required(route)` 讓 guard 和 `can_activate` 的判斷一致（例如略過 `#[meta(Public)]`）。
- **錯誤回應統一是 `ProblemDetails`**：hornero 的錯誤一律是 problem+json，所以 4xx、5xx 的 `#[api_response]` 不需要指定 body。
- **feature 關閉時 attribute 仍然被接受**：`#[routes]` 永遠讀取並移除 `#[api_*]`，只有開啟 `openapi` 時才產生程式碼，同一份程式碼在兩種設定下都能編譯。
- 文件以 `OpenApiModule::for_root` 掛上，相當於 `SwaggerModule.setup`；Swagger UI 用 `utoipa-swagger-ui` 的 vendored 檔案，build 時不需要網路。

## 理由

- 路徑只有一個來源。路由表就是 axum 實際使用的路徑，文件不可能和路由不一致。
- 簽名本來就描述了 request 的形狀，推導讓文件的成本接近零，和 Nest 的 CLI plugin 一樣是預設行為，而不是額外的工作。
- autoref specialization 讓推導在 feature unification 下也安全：依賴圖中任何一個 crate 開了 `openapi`，所有 crate 的 derive 都會產生 `describe`，但不會因此出現新的 trait bound 或編譯錯誤。
- hornero-core 保持與文件格式無關。路由表與 `describe` 也能用來產生其他格式（例如 AsyncAPI 或路由清單）。

## 後果

- 推導只看語法：型別別名、自訂 extractor 與重新命名的 import 認不出來，需要 `#[api_body]` 或 `#[api_response(body = ..)]`。
- garde 的驗證規則不會轉成 schema 限制，要再用 utoipa 的 `#[schema(..)]` 寫一次。
- 泛型 body（`Page<Todo>`）直接展開，不放進 `components.schemas`，因為 utoipa 的 `ToSchema::name` 無法區分泛型的不同實例。
- 使用者必須自己依賴 `utoipa` 6：它的 derive 產生的程式碼引用 `utoipa` 路徑。utoipa 6 要求 Rust 1.88，workspace 的 MSRV 因此從 1.85 提高到 1.88。
- `mount_router` 與 `mount` 掛上的 axum router 不在路由表裡，也不在文件裡；需要時用 `OpenApiConfig::modify` 補上。

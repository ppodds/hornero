# 14. 常見做法

## Log 與 tracing

hornero 透過 [`tracing`](https://docs.rs/tracing) 輸出 log。在 `main` 安裝一個 subscriber 才看得到：

```rust,no_run
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()) // RUST_LOG=info,hornero_core=debug
    .init();
```

hornero 會輸出：

| 層級 | 內容 |
|---|---|
| `info` | 開始 listen 的位址、scope report、收到關閉訊號 |
| `error` | 5xx 錯誤（含完整的 source chain）、handler panic、失敗的生命週期 hook |

每個 request 的 log 請用 `tower_http::trace::TraceLayer`（見下一節）。需要 handler 名稱時，可以寫一個 interceptor（見 [Request pipeline](05-request-pipeline.md#interceptor)）。

## 常用的 tower layer：trace、CORS、timeout

```rust
use std::time::Duration;

use hornero::prelude::*;
use tower_http::cors::CorsLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

#[module]
struct AppModule;

let _hornero = Hornero::new(AppModule)
    .layer(TraceLayer::new_for_http())
    .layer(CorsLayer::permissive()) // 正式環境請明確列出允許的來源
    .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)));
```

`Hornero::layer` 包住整個應用，**先加的在外層**。只想套在某個模組上時，在該模組的 `define` 裡用 `m.layer(...)`。

## Request ID

用 tower-http 產生 request id，並在 handler 或 request-scoped provider 裡讀取：

```rust
use hornero::prelude::*;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer};

#[controller("/whoami")]
struct WhoAmIController;

#[routes]
impl WhoAmIController {
    #[get("/")]
    fn whoami(&self, Extension(id): Extension<RequestId>) -> String {
        id.header_value().to_str().unwrap_or_default().to_owned()
    }
}

#[module(controllers = [WhoAmIController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let hornero = Hornero::new(AppModule)
    .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid)) // 外層：沒有 x-request-id 時產生一個
    .layer(PropagateRequestIdLayer::x_request_id());        // 內層：把它複製到回應

let app = hornero_testing::TestApp::from_hornero(hornero).build().await.unwrap();
let res = app.client().get("/whoami").header("x-request-id", "abc-123").await;
assert_eq!(res.text(), "abc-123");
assert_eq!(res.header("x-request-id"), Some("abc-123"));
# }
```

request-scoped provider 可以用 `ctx.get::<RequestId>()`（`ctx` 是注入的 `Arc<RequestContext>`）取得同一個值，例如替 audit log 加上 request id。

## 統一回應格式（envelope）

Nest 的 `TransformInterceptor` 在 hornero 裡是一個會改寫 body 的 interceptor：

```rust
use hornero::axum::body::{Body, to_bytes};
use hornero::axum::extract::Request;
use hornero::axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hornero::axum::middleware::Next;
use hornero::prelude::*;
use serde_json::{Value, json};

#[derive(Injectable)]
struct Envelope;

impl Interceptor for Envelope {
    async fn intercept(&self, _ctx: &RouteContext, req: Request, next: Next) -> Response {
        let response = next.run(req).await;
        let is_json = response
            .headers()
            .get(CONTENT_TYPE)
            .is_some_and(|value| value == "application/json");
        if !response.status().is_success() || !is_json {
            return response; // 錯誤與非 JSON 回應保持原樣
        }
        let (mut parts, body) = response.into_parts();
        let data: Value = match to_bytes(body, 10 * 1024 * 1024).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            Err(err) => return HttpError::from_boxed(err.into()).into_response(),
        };
        parts.headers.remove(CONTENT_LENGTH);
        Response::from_parts(parts, Body::from(json!({ "data": data }).to_string()))
    }
}

#[controller("/items")]
#[interceptors(Envelope)]
struct ItemController;

#[routes]
impl ItemController {
    #[get("/")]
    fn list(&self) -> Json<Vec<&'static str>> {
        Json(vec!["apple", "pear"])
    }
}

#[module(controllers = [ItemController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let body: Value = app.client().get("/items").await.json();
assert_eq!(body, json!({ "data": ["apple", "pear"] }));
# }
```

要套用到所有路由時，在模組的 `providers` 加上 `AppInterceptor<Envelope>`，或改用 `Hornero::new(AppModule).interceptor::<Envelope>()`。

## 驗證身分與權限

把以下三者組合起來：

1. **`AuthGuard`** 驗證 token，把使用者寫進 extensions。
2. **`AuthUser` extractor** 從 extensions 取出使用者，讓 handler 以型別宣告「我需要已登入的使用者」。
3. **`RolesGuard` 搭配 `#[meta(Roles(&["admin"]))]`** 檢查權限。

完整程式碼見 [Request pipeline](05-request-pipeline.md#guard) 與 `examples/todo/src/auth/mod.rs`。若大多數路由都需要登入，在 `AuthModule` 的 `providers` 加上 `AppGuard<AuthGuard>` 讓它成為全域 guard（Nest 的 `APP_GUARD`），再用 `Public` metadata 標出公開路由：

```rust
use hornero::prelude::*;
# #[derive(Injectable)]
# struct TokenStore;
# #[derive(Injectable)]
# struct AuthGuard {
#     tokens: Arc<TokenStore>,
# }
# impl Guard for AuthGuard {
#     async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> { Ok(()) }
# }

#[module(providers = [TokenStore, AuthGuard, AppGuard<AuthGuard>], exports = [AuthGuard])]
pub struct AuthModule;
# Hornero::new(AuthModule).validate().unwrap();
```

`AppGuard<AuthGuard>` 在 `AuthModule` 裡解析，所以會用到 `AuthModule` 自己的 `AuthGuard` 實例與私有的 `TokenStore`；其他模組不需要 import `AuthModule`，路由也會受保護。

## 背景工作

在 `on_application_bootstrap` 啟動工作，以注入的 `Arc<Shutdown>` 得知何時停止，在 `on_module_destroy` 等待它結束。完整範例見 [生命週期](07-lifecycle.md#shutdown背景工作與主動關閉)。

## 多個資料庫

用泛型 tag：`SeaOrmModule::for_root(url).tag::<Replica>()` 提供 `Arc<Db<Replica>>`。見 [資料庫](12-database.md#多個資料庫)。

## 嵌入既有的 axum 應用

`App::router()` 回傳一般的 axum `Router`，可以 merge 或 nest 進既有的應用：

```rust
use hornero::axum::Router;
use hornero::axum::body::Body;
use hornero::axum::http::Request;
use hornero::axum::routing::get;
use hornero::prelude::*;
use hornero::tower::ServiceExt;

#[controller("/users")]
struct UserController;

#[routes]
impl UserController {
    #[get("/")]
    fn list(&self) -> &'static str {
        "[]"
    }
}

#[module(controllers = [UserController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() -> Result<(), BoxError> {
let hornero_app = Hornero::new(AppModule).build().await?;

let router = Router::new()
    .route("/legacy", get(|| async { "still here" }))
    .nest("/v2", hornero_app.router());

let res = router
    .oneshot(Request::get("/v2/users").body(Body::empty())?)
    .await?;
assert_eq!(res.status(), StatusCode::OK);

// 自己 serve router 時，結束前記得執行關閉 hook
hornero_app.close().await?;
# Ok(())
# }
```

hornero 的 404 fallback 只在 hornero 自己的 router 裡生效。嵌入後，沒有符合的路徑由外層 router 決定怎麼處理。

## 靜態檔案

用 tower-http 的 `ServeDir`（需要 `fs` feature），透過 `mount_router` 掛進模組：

```rust
use hornero::prelude::*;
use hornero::axum::Router;
use tower_http::services::ServeDir;
# struct WebModule;

impl Module for WebModule {
    fn define(&self, m: &mut ModuleDef) {
        m.mount_router("/assets", Router::new().fallback_service(ServeDir::new("public")));
    }
}
```

掛在 `/` 的 router 可以帶自己的 fallback，例如單頁應用（SPA）：controller 路由照常運作，其餘路徑交給前端的 `index.html`。只有沒被任何 fallback 接手的請求，才會得到 hornero 的 problem+json 404。

這個 fallback 和 404 一樣只經過全域 middleware 與 layer，不經過模組的 `m.middleware` / `m.layer`（它們只包住模組的路由）。fallback 需要的 layer（例如快取 header）請直接加在掛進去的 `Router` 上。

```rust
use hornero::prelude::*;
use hornero::axum::Router;
use tower_http::services::{ServeDir, ServeFile};
# struct WebModule;

impl Module for WebModule {
    fn define(&self, m: &mut ModuleDef) {
        m.mount_router(
            "/",
            Router::new()
                .fallback_service(ServeDir::new("dist").fallback(ServeFile::new("dist/index.html"))),
        );
    }
}
```

## 每個環境不同的設定

`Source` 的後者覆寫前者，因此常見的分層是：程式內預設值 → `config.toml` → 環境變數。

```rust
use hornero_config::{ConfigModule, Source};
# #[derive(Default, serde::Serialize, serde::Deserialize)]
# struct AppConfig {}

ConfigModule::for_root(
    Source::new()
        .defaults(AppConfig::default())
        .toml("config.toml")
        .env("APP_"), // 部署時用 APP_DATABASE__URL 等環境變數覆寫
)
.load::<AppConfig>()
# ;
```

## 在測試中啟動真正的 server

大多數測試用 `TestApp` 就夠了。需要真正的 TCP 連線時（例如測試 WebSocket 或 HTTP client library）：

```rust
use hornero::prelude::*;
use tokio::net::TcpListener;

#[module]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() -> Result<(), BoxError> {
let app = Hornero::new(AppModule).build().await?;
let shutdown = app.shutdown_handle();
let listener = TcpListener::bind("127.0.0.1:0").await?; // 隨機 port
let address = listener.local_addr()?;
let server = tokio::spawn(app.serve(listener));

// ... 對 http://{address} 發出 request ...
println!("listening on {address}");

shutdown.trigger(); // 停止 server 並執行關閉 hook
server.await??;
# Ok(())
# }
```

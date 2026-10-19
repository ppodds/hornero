# 5. Request pipeline：guard、interceptor、filter、middleware

hornero 有四種可注入的 enhancer，加上 tower layer。它們都可以有依賴，也都會在啟動時驗證。

| 元件 | 知道是哪條路由嗎 | 用途 |
|---|---|---|
| **Guard** | 是 | 決定 request 能不能進到 handler（驗證身分、權限、功能開關） |
| **Interceptor** | 是 | 包住 handler：計時、log、快取、修改回應 |
| **Exception filter** | 是 | 重新渲染錯誤回應 |
| **Middleware** | 否 | 需要 DI、但與路由無關的處理（API key、request id、租戶解析） |
| **tower layer** | 否 | 不需要 DI 的通用處理（trace、CORS、壓縮、timeout），可直接使用整個 tower-http 生態系 |

## 執行順序

由外到內：

```text
5xx log                                         最外層，依最終回應記錄
全域 middleware      Hornero::middleware            包住所有路由與 404 fallback
全域 tower layer     Hornero::layer
CatchPanicLayer                                 panic → 500 problem+json
  模組 middleware    #[module(middlewares = [..])]  只包住該模組的路由與 mount
  模組 tower layer   ModuleDef::layer
    request scope                               只有用到 request-scoped 元件的路由才有
    exception filter                            最具體、且 catches 該錯誤的那一個
    guards           global → controller → handler
    interceptors     global ⊃ controller ⊃ handler
    extractors → handler
```

同一組裡，**先註冊的在外層**。`CatchPanicLayer` 在全域 middleware 與 layer 的內側，所以 panic 產生的 500 也會經過它們（CORS header、request id、`TraceLayer` 都看得到）；它仍在路由之外，連 404 fallback 都涵蓋。

這個順序帶來幾個結果：

- **filter 包住 guard 與 interceptor**，所以 guard 回傳的 401 / 403 也會經過 filter。
- **interceptor 在 guard 裡面**，所以被 guard 擋下的 request 不會經過 interceptor。要記錄每一個 request，請用 `TraceLayer` 或 middleware。
- **method 不符的 405 不會執行任何 guard、interceptor 或 filter**：route 層只對該路由註冊的 method 生效，405 以 problem+json 回應並保留 `Allow` header。
- **middleware 不知道路由**：它在路由比對前後都看不到 controller 或 handler 的資訊。需要路由資訊時，請用 guard 或 interceptor。

## Guard

<!-- ignore: trait 簽章摘要，真正的定義在 hornero-core -->
```rust,ignore
pub trait Guard: Send + Sync + 'static {
    fn can_activate(&self, ctx: &mut GuardContext<'_>) -> impl Future<Output = Result<(), HttpError>> + Send;
}

pub struct GuardContext<'a> {
    pub route: &'a RouteContext,  // controller、handler、method、路徑、metadata
    pub parts: &'a mut Parts,     // request head，可以讀寫 extensions
}
```

guard 回傳 `Result` 而不是 `bool`，所以 guard 可以自己決定回 401 還是 403，以及錯誤訊息。guard 可以把資料寫進 `parts.extensions`，例如已驗證的使用者，讓之後的 guard、extractor 與 request-scoped provider 讀取。

以下範例包含一個驗證 bearer token 的 guard、一個讀取 metadata 的權限 guard，以及一個「proof」extractor：

```rust
use hornero::axum::extract::FromRequestParts;
use hornero::axum::http::request::Parts;
use hornero::prelude::*;

/// 已驗證的呼叫者。handler 參數裡有它，就證明 request 通過了 AuthGuard。
#[derive(Clone)]
struct AuthUser {
    name: String,
    admin: bool,
}

impl<S: Send + Sync> FromRequestParts<S> for AuthUser {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, HttpError> {
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .ok_or_else(|| HttpError::unauthorized("authentication required"))
    }
}

#[derive(Injectable)]
struct TokenStore;

impl TokenStore {
    fn verify(&self, token: &str) -> Option<AuthUser> {
        match token {
            "admin-token" => Some(AuthUser { name: "alice".into(), admin: true }),
            "user-token" => Some(AuthUser { name: "bob".into(), admin: false }),
            _ => None,
        }
    }
}

#[derive(Injectable)]
struct AuthGuard {
    tokens: Arc<TokenStore>, // guard 也是 provider，可以有依賴
}

impl Guard for AuthGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let token = ctx
            .parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| HttpError::unauthorized("missing bearer token"))?;
        let user = self
            .tokens
            .verify(token)
            .ok_or_else(|| HttpError::unauthorized("invalid token"))?;
        ctx.parts.extensions.insert(user);
        Ok(())
    }
}

/// Route metadata：這條路由只有管理員能用。
struct AdminOnly;

#[derive(Injectable)]
struct AdminGuard;

impl Guard for AdminGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        if ctx.route.meta::<AdminOnly>().is_none() {
            return Ok(());
        }
        match ctx.parts.extensions.get::<AuthUser>() {
            Some(user) if user.admin => Ok(()),
            Some(_) => Err(HttpError::forbidden("administrators only")),
            None => Err(HttpError::unauthorized("authentication required")),
        }
    }
}

#[controller("/accounts")]
#[guards(AuthGuard, AdminGuard)]
struct AccountController;

#[routes]
impl AccountController {
    #[get("/me")]
    fn me(&self, user: AuthUser) -> String {
        user.name
    }

    #[delete("/{name}")]
    #[meta(AdminOnly)]
    fn remove(&self, Path(name): Path<String>) -> String {
        format!("removed {name}")
    }
}

#[module(controllers = [AccountController], providers = [TokenStore, AuthGuard, AdminGuard])]
struct AccountModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AccountModule).build().await.unwrap();
let client = app.client();

let anonymous = client.get("/accounts/me").await;
assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
assert_eq!(anonymous.header("www-authenticate"), Some("Bearer"));

assert_eq!(client.get("/accounts/me").bearer("user-token").await.text(), "bob");
assert_eq!(client.delete("/accounts/carol").bearer("user-token").await.status(), StatusCode::FORBIDDEN);
assert_eq!(client.delete("/accounts/carol").bearer("admin-token").await.text(), "removed carol");
# }
```

### 註冊與解析

| 範圍 | 寫法 |
|---|---|
| 整個 controller | `#[controller]` 下方加 `#[guards(AuthGuard)]`，或 `c.guard::<AuthGuard>()` |
| 單一路由 | `#[guards(RolesGuard)]` 或 `c.get(..).guard::<RolesGuard>()` |
| 全部路由，在模組裡宣告 | `#[module(providers = [AppGuard<AuthGuard>])]`，相當於 Nest 的 `{ provide: APP_GUARD, useClass: AuthGuard }` |
| 全部路由，在啟動時設定 | `Hornero::new(AppModule).guard::<AuthGuard>()` |

guard 依序執行：global、controller、handler，第一個回傳錯誤的 guard 會中止後續流程。global guard 裡，`Hornero::guard` 註冊的先執行，接著是模組宣告的 `AppGuard`：被 import 的模組先於 import 它的模組。

hornero 解析 guard 實例的規則和一般依賴相同：如果 controller 所屬的模組看得到 `AuthGuard` provider（自己提供的，或 import 進來的 export），就使用那個實例。看不到時，hornero 會在該模組建立一個**私有的隱式實例**（和 Nest 的 `@UseGuards(X)` 相同），此時 guard 的依賴必須在該模組可見。global guard 則在註冊它的地方解析：`Hornero::guard` 在根模組，`AppGuard<G>` 在宣告它的模組，所以可以依賴那個模組沒有 export 的 provider。

實務上，常見做法是讓 `AuthModule` 提供並 export guard，功能模組 import `AuthModule` 就能使用，不必知道 `TokenStore` 這類細節。`examples/todo/src/auth` 就是這樣寫的。要讓所有路由都需要登入時，在 `AuthModule` 的 `providers` 加上 `AppGuard<AuthGuard>`，功能模組連 import 都不用。

### 全域 guard 與公開路由

把驗證設成全域，再用 metadata 標出公開的路由。這裡用 `Hornero::guard` 註冊；在 `AppModule` 的 `providers` 寫 `AppGuard<ApiKeyGuard>` 效果相同：

```rust
use hornero::prelude::*;

/// 標記不需要 API key 的路由
struct Public;

#[derive(Injectable)]
struct ApiKeyGuard;

impl Guard for ApiKeyGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        if ctx.route.meta::<Public>().is_some() {
            return Ok(());
        }
        match ctx.parts.headers.get("x-api-key") {
            Some(key) if key == "secret" => Ok(()),
            _ => Err(HttpError::unauthorized("missing or invalid api key")),
        }
    }
}

#[controller("/health")]
#[meta(Public)]
struct HealthController;

#[routes]
impl HealthController {
    #[get("/")]
    fn health(&self) -> &'static str {
        "ok"
    }
}

#[controller("/reports")]
struct ReportController;

#[routes]
impl ReportController {
    #[get("/")]
    fn list(&self) -> &'static str {
        "[]"
    }
}

#[module(controllers = [HealthController, ReportController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let hornero = Hornero::new(AppModule).guard::<ApiKeyGuard>();

let app = hornero_testing::TestApp::from_hornero(hornero).build().await.unwrap();
let client = app.client();
assert_eq!(client.get("/health").await.status(), StatusCode::OK);
assert_eq!(client.get("/reports").await.status(), StatusCode::UNAUTHORIZED);
assert_eq!(client.get("/reports").header("x-api-key", "secret").await.status(), StatusCode::OK);
# }
```

## Metadata

metadata 以型別作為 key，取代 Nest 的 `SetMetadata` 加 `Reflector`：

- 設定：在 controller（`#[controller]` 下方）或路由方法上加 `#[meta(Roles(&["admin"]))]`，或用 builder 的 `.meta(value)`。
- 讀取：guard 用 `ctx.route.meta::<Roles>()`，interceptor 與 filter 用 `ctx.meta::<Roles>()`。
- 同一型別同時設在 controller 與 handler 上時，handler 的值優先。

metadata 型別只需要 `Send + Sync + 'static`。用 newtype 包裝值，就不會和別人的 metadata 撞名。

## Interceptor

<!-- ignore: trait 簽章摘要，真正的定義在 hornero-core -->
```rust,ignore
pub trait Interceptor: Send + Sync + 'static {
    fn intercept(&self, ctx: &RouteContext, req: Request, next: Next) -> impl Future<Output = Response> + Send;
}
```

Nest 用 RxJS 寫的 interceptor，在 hornero 裡就是一個 `async fn(req, next) -> Response`：

- 呼叫 `next.run(req).await` 之前可以修改 request，之後可以修改回應。
- 不呼叫 `next` 就能直接回應（例如快取命中、維護模式）。
- 全域 interceptor 包住 controller 的，controller 的包住 handler 的。全域 interceptor 可以用 `Hornero::interceptor::<I>()` 註冊，或在模組的 `providers` 寫 `AppInterceptor<I>`（Nest 的 `APP_INTERCEPTOR`）；前者在外層。

```rust
use std::time::Instant;

use hornero::axum::extract::Request;
use hornero::axum::http::HeaderValue;
use hornero::axum::middleware::Next;
use hornero::prelude::*;

#[derive(Injectable)]
struct Timing;

impl Interceptor for Timing {
    async fn intercept(&self, ctx: &RouteContext, req: Request, next: Next) -> Response {
        let started = Instant::now();
        let mut response = next.run(req).await; // 執行內層 interceptor 與 handler
        let elapsed = started.elapsed().as_micros().to_string();
        response
            .headers_mut()
            .insert("x-elapsed-us", HeaderValue::from_str(&elapsed).unwrap());
        tracing::debug!(handler = ctx.handler(), elapsed_us = %elapsed, "handled");
        response
    }
}

/// metadata：要加上的 Cache-Control 值
struct CacheFor(&'static str);

#[derive(Injectable)]
struct CacheControl;

impl Interceptor for CacheControl {
    async fn intercept(&self, ctx: &RouteContext, req: Request, next: Next) -> Response {
        let mut response = next.run(req).await;
        if let Some(CacheFor(value)) = ctx.meta::<CacheFor>() {
            response
                .headers_mut()
                .insert("cache-control", HeaderValue::from_static(value));
        }
        response
    }
}

#[controller("/catalog")]
#[interceptors(Timing)]
struct CatalogController;

#[routes]
impl CatalogController {
    #[get("/")]
    #[interceptors(CacheControl)]
    #[meta(CacheFor("max-age=60"))]
    fn list(&self) -> &'static str {
        "[]"
    }
}

#[module(controllers = [CatalogController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let res = app.client().get("/catalog").await;
assert_eq!(res.header("cache-control"), Some("max-age=60"));
assert!(res.header("x-elapsed-us").is_some());
# }
```

## Exception filter

<!-- ignore: trait 簽章摘要，真正的定義在 hornero-core；預設方法的內容省略 -->
```rust,ignore
pub trait ExceptionFilter: Send + Sync + 'static {
    fn catches(&self, err: &HttpError) -> bool { true } // 選用，相當於 @Catch(Type)
    fn catch(&self, err: &HttpError, ctx: &RouteContext) -> impl Future<Output = Response> + Send;
}
```

Rust 沒有 exception。handler 回傳 `Result<_, HttpError>`，`HttpError` 轉成回應時會把自己附在回應的 extensions 上，filter 就是靠這個找到原始錯誤並重新渲染。

- **每個錯誤由一個 filter 處理**：依序查看 handler 層、controller 層、全域的 filter（同一層依註冊順序；全域的先看 `Hornero::filter::<F>()`，再看模組 `providers` 裡的 `AppFilter<F>`，即 Nest 的 `APP_FILTER`），第一個 `catches` 回傳 `true` 的處理它。沒有 filter 接住時，錯誤以預設格式回應。
- **依錯誤型別接住**：用 `?` 拋出的 domain error 會變成 500，原始錯誤保留為 source。`catches` 裡用 `err.is::<TodoError>()` 判斷，`catch` 裡用 `err.downcast_ref::<TodoError>()` 取出，就能把它轉成 404、409 等回應。完整範例見 [錯誤處理](08-errors.md#domain-error)。
- **filter 看得到**：handler、guard、interceptor 回傳的 `HttpError`；`Rejection = HttpError` 的 extractor（包括 `hornero-validation` 的 `Valid`）；request-scoped 元件建構失敗產生的 500。
- **filter 看不到**：axum 內建 extractor 的 rejection（純文字，例如格式錯誤的 `Json`）、404 fallback、panic。要讓輸入錯誤也走 filter，請使用 `Valid<Json<T>>` 或 `Rejection = HttpError` 的自訂 extractor。

```rust
use hornero::axum::http::header::CONTENT_TYPE;
use hornero::prelude::*;
use serde_json::json;

/// 在預設的 problem document 上補上出錯的路由
#[derive(Injectable)]
struct ProblemFilter;

impl ExceptionFilter for ProblemFilter {
    async fn catch(&self, err: &HttpError, ctx: &RouteContext) -> Response {
        let mut problem = err.to_problem();
        problem["instance"] = json!(ctx.path());
        problem["handler"] = json!(format!("{}::{}", ctx.controller(), ctx.handler()));
        (err.status(), [(CONTENT_TYPE, "application/problem+json")], problem.to_string()).into_response()
    }
}

#[controller("/orders")]
struct OrderController;

#[routes]
impl OrderController {
    #[get("/{id}")]
    fn find(&self, Path(id): Path<u64>) -> Result<String, HttpError> {
        Err(HttpError::not_found(format!("order {id} not found")))
    }
}

#[module(controllers = [OrderController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let hornero = Hornero::new(AppModule).filter::<ProblemFilter>();

let app = hornero_testing::TestApp::from_hornero(hornero).build().await.unwrap();
let problem: serde_json::Value = app.client().get("/orders/7").await.json();
assert_eq!(problem["status"], 404);
assert_eq!(problem["detail"], "order 7 not found");
assert_eq!(problem["instance"], "/orders/{id}");
assert_eq!(problem["handler"], "OrderController::find");
# }
```

filter 產生的回應會**完全取代**原本的回應。`HttpError` 攜帶的 header（例如 401 預設的 `WWW-Authenticate: Bearer`）不會自動保留，自訂 filter 若想保留，可以從 `err.render()`（預設回應）開始修改，或自己組回應時複製 `err.headers()`。

## Middleware

<!-- ignore: trait 簽章摘要，真正的定義在 hornero-core -->
```rust,ignore
pub trait Middleware: Send + Sync + 'static {
    fn handle(&self, req: Request, next: Next) -> impl Future<Output = Response> + Send;
}
```

需要注入依賴、但與路由無關的處理就用 middleware：

```rust
use hornero::axum::extract::Request;
use hornero::axum::http::HeaderValue;
use hornero::axum::middleware::Next;
use hornero::prelude::*;

struct ServerName(&'static str);

#[derive(Injectable)]
struct PoweredBy {
    name: Arc<ServerName>,
}

impl Middleware for PoweredBy {
    async fn handle(&self, req: Request, next: Next) -> Response {
        let mut response = next.run(req).await;
        response
            .headers_mut()
            .insert("x-powered-by", HeaderValue::from_static(self.name.0));
        response
    }
}

#[controller("/ping")]
struct PingController;

#[routes]
impl PingController {
    #[get("/")]
    fn ping(&self) -> &'static str {
        "pong"
    }
}

#[module(
    controllers = [PingController],
    providers = [value(ServerName("hornero"))],
    middlewares = [PoweredBy], // 只包住這個模組的路由
)]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
assert_eq!(app.client().get("/ping").await.header("x-powered-by"), Some("hornero"));
# }
```

- **模組 middleware**（`middlewares = [..]` 或 `m.middleware::<M>()`）只包住該模組的 controller 與 mount 的路由。沒有路由符合的 request 不經過任何模組的 middleware，即使它交給了該模組掛在 `/` 的 router 的 fallback。
- **全域 middleware**（`Hornero::middleware::<M>()`）包住所有路由，包括 404 fallback。
- middleware 在啟動時就要實例化，所以**必須是 singleton**。如果它依賴 request-scoped 的東西而被傳染，啟動會失敗。

## tower layer

不需要 DI 的處理，直接使用 tower layer：

```rust
use hornero::prelude::*;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

#[module]
struct AppModule;

let _hornero = Hornero::new(AppModule)
    .layer(TraceLayer::new_for_http()) // 最外層
    .layer(CorsLayer::permissive());
```

- `Hornero::layer` 包住整個應用（包括 404 fallback），先加的在外層。
- `ModuleDef::layer` 只包住該模組的路由，例如只對 admin 模組加上 rate limit。
- 同一層級裡，DI middleware 在 tower layer 外面。
- `axum::middleware::from_fn` 產生的 layer 也可以直接傳給 `layer`。

## 該用哪一個？

| 需求 | 選擇 |
|---|---|
| 擋下沒有權限的 request | Guard |
| handler 需要已驗證的使用者 | Guard 寫入 extensions，加上 proof extractor |
| 依路由 metadata 決定行為（快取、權限、速率） | Guard 或 Interceptor |
| 計時、log handler 名稱、修改回應 | Interceptor |
| 統一錯誤格式、把 domain error 轉成狀態碼 | Exception filter |
| 需要依賴、與路由無關（API key 驗證、租戶解析） | Middleware |
| 通用 HTTP 處理（trace、CORS、壓縮、timeout） | tower layer |

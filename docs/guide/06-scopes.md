# 6. Scope 與 request 資料

scope 決定一個 provider 有幾個實例：

| Scope | 實例數量 |
|---|---|
| `Singleton`（預設） | 整個應用一個 |
| `Transient` | 每個使用者（consumer）一個 |

不論哪一種，**所有實例都在啟動時建構**。request 期間不會建構任何 provider，所以建構失敗一定發生在 bind port 之前，不會變成某個 request 的 500。

hornero 沒有 Nest 的 `Scope.REQUEST`：request 的資料不進依賴圖，而是走 axum 的 extractor，見 [Request 資料](#request-資料)。為什麼這樣取捨，記錄在 [ADR-0007](../adr/0007-no-request-scope.md)。

## 宣告 scope

```rust
use hornero::prelude::*;
use hornero::ProviderDef;
# struct TraceId(u64);
# async fn make_trace_id() -> Result<TraceId, BoxError> { Ok(TraceId(7)) }

#[derive(Injectable)]
#[inject(scope = transient)]
struct Scratchpad { /* ... */ }

struct Nonce;

#[injectable(scope = transient)]
impl Nonce {
    fn new() -> Result<Self, BoxError> { /* ... */ Ok(Nonce) }
}

// 手寫或 factory provider
# struct ManualModule;
# impl Module for ManualModule {
#     fn define(&self, m: &mut ModuleDef) {
m.provider(ProviderDef::factory(make_trace_id).scope(Scope::Transient));
#     }
# }
// 在 #[module] 裡
#[module(providers = [provider(ProviderDef::factory(make_trace_id).scope(Scope::Transient))])]
struct TraceModule;
```

## Transient

transient provider 每被一個元件依賴，就建構一個新實例。同一個元件就算把同一型別宣告兩次，也只會拿到同一個實例：

```rust
use std::sync::atomic::{AtomicU64, Ordering};

use hornero::prelude::*;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct TraceId(u64);

#[injectable(scope = transient)]
impl TraceId {
    fn new() -> Self {
        TraceId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Injectable)]
struct OrderService {
    trace: Arc<TraceId>,
}

#[derive(Injectable)]
struct BillingService {
    trace: Arc<TraceId>,
}

#[module(providers = [TraceId, OrderService, BillingService])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
let orders = app.get::<OrderService>().unwrap();
let billing = app.get::<BillingService>().unwrap();
assert_ne!(orders.trace.0, billing.trace.0);
# }
```

依賴 transient provider 的元件不會改變 scope：它們各自持有一個實例，在啟動時建構好。

## Scope 的限制

以下情況需要唯一的實例，宣告成 transient 會在啟動時回報 `scope violation`：

**有生命週期 hook 的 provider 必須是 singleton**：Nest 會靜默地不呼叫 hook，hornero 改為直接報錯。

```text
[1] scope violation
    Ephemeral (in App) has lifecycle hooks but is transient; hooks only run on singletons.
```

**controller 必須是 singleton**：路由在啟動時組裝，controller 只建構一次，由所有 request 共用。

```text
[1] scope violation
    Ephemeral (in App) cannot be transient; a controller is built once, during bootstrap.
```

guard、interceptor、filter 與 middleware 可以是 transient：每個用到它的模組各有一個實例，同樣在啟動時建構。

## Request 資料

request 的資料放在 request 自己身上：guard、interceptor 與 middleware 把資料寫進 extensions，handler 用 extractor 讀取。

### 在 handler 讀取，再傳給 service

```rust
use hornero::axum::extract::FromRequestParts;
use hornero::axum::http::request::Parts;
use hornero::prelude::*;

/// 已驗證的呼叫者。handler 參數裡有它，就證明 request 通過了 IdentityGuard。
#[derive(Clone)]
struct CurrentUser {
    name: String,
}

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, HttpError> {
        parts
            .extensions
            .get::<CurrentUser>()
            .cloned()
            .ok_or_else(|| HttpError::unauthorized("not authenticated"))
    }
}

/// 假裝是資料庫
struct Directory;

impl Directory {
    fn find(&self, token: &str) -> Option<CurrentUser> {
        (token == "ada-token").then(|| CurrentUser { name: "ada".into() })
    }
}

#[derive(Injectable)]
struct IdentityGuard {
    directory: Arc<Directory>,
}

impl Guard for IdentityGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let user = ctx
            .parts
            .headers
            .get("x-token")
            .and_then(|value| value.to_str().ok())
            .and_then(|token| self.directory.find(token))
            .ok_or_else(|| HttpError::unauthorized("invalid token"))?;
        ctx.parts.extensions.insert(user);
        Ok(())
    }
}

/// 不保存 request 的狀態：使用者由參數傳入，所以一個實例可以服務所有 request
#[derive(Injectable)]
struct Greeter;

impl Greeter {
    fn greet(&self, user: &CurrentUser) -> String {
        format!("hello, {}", user.name)
    }
}

#[controller("/greet")]
#[guards(IdentityGuard)]
struct GreetController {
    greeter: Arc<Greeter>,
}

#[routes]
impl GreetController {
    #[get("/")]
    fn greet(&self, user: CurrentUser) -> String {
        self.greeter.greet(&user)
    }
}

#[module(
    controllers = [GreetController],
    providers = [Greeter, IdentityGuard, value(Directory)],
)]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let client = app.client();
assert_eq!(client.get("/greet").header("x-token", "ada-token").await.text(), "hello, ada");
assert_eq!(client.get("/greet").await.status(), StatusCode::UNAUTHORIZED);
# }
```

幾個重點：

- **handler 簽名就是依賴宣告**：`user: CurrentUser` 表示這個 handler 需要已驗證的使用者，型別本身就證明資料存在。
- **service 用參數接收 request 資料**：呼叫鏈上每一層都看得到自己依賴什麼。如果很多方法都要同一份資料，把它們包成一個 context struct 傳遞，一樣不需要進依賴圖。
- **需要 DI 的工作放在 guard、interceptor 或 middleware**：它們是 provider，可以注入依賴。extractor 拿不到 provider（handler 的 state 是 `()`），所以它只負責從 extensions 讀出已經準備好的資料。上例的 `Directory` 查詢就是這樣分工的。

### 依 request 資料呈現錯誤

exception filter 只看得到錯誤與路由：它執行時 request 已經被 handler 消耗了。回應要依 request 的內容變化時，改用手上還有 request 的 interceptor 或 middleware：在 `next.run` 之前讀好需要的資料，之後用 `HttpError::in_response` 取得錯誤。

```rust
use hornero::axum::body::Body;
use hornero::axum::extract::Request;
use hornero::axum::http::HeaderValue;
use hornero::axum::http::header::{ACCEPT, CONTENT_TYPE};
use hornero::axum::middleware::Next;
use hornero::prelude::*;

/// 瀏覽器要 HTML 時，把錯誤渲染成頁面
#[derive(Injectable)]
struct HtmlErrors;

impl Middleware for HtmlErrors {
    async fn handle(&self, req: Request, next: Next) -> Response {
        let wants_html = req.headers().get(ACCEPT).is_some_and(|value| value == "text/html");
        let mut response = next.run(req).await;
        if wants_html && let Some(err) = HttpError::in_response(&response) {
            let page = format!("<h1>{}</h1>", err.message());
            response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
            *response.body_mut() = Body::from(page);
        }
        response
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

#[module(controllers = [OrderController], middlewares = [HtmlErrors])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let client = app.client();

let page = client.get("/orders/7").header("accept", "text/html").await;
assert_eq!(page.status(), StatusCode::NOT_FOUND);
assert_eq!(page.text(), "<h1>order 7 not found</h1>");

let problem: serde_json::Value = client.get("/orders/7").await.json();
assert_eq!(problem["detail"], "order 7 not found");
# }
```

| 用 | 看得到 | 看不到 |
|---|---|---|
| interceptor | guard 寫入的資料（例如已驗證的使用者） | guard 的拒絕：被擋下的 request 不會經過 interceptor |
| middleware | 所有錯誤，包括 guard 的 401 / 403 | guard 寫入的資料：request 在 middleware 交出去之後才經過 guard |

像上例這樣原地修改 response，`HttpError` 會留在 response 上，5xx 的 log 照常記錄。回傳全新的 response 則會連 `HttpError` 一起丟掉，這筆 5xx 就不會被 hornero 記錄。interceptor 在 filter 內層，所以錯誤還留在 response 上時，route 上的 filter 仍會接手渲染。

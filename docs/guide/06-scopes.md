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
- **guard 負責擋下 request**：`IdentityGuard` 是 provider，可以注入 `Directory`；驗證失敗的 request 不會進 handler，通過時把結果寫進 extensions，extractor 只負責讀出來。結果只有 handler 需要，或不是每條路由都需要時，改用下一節的 `Extractor`。

### 需要 provider 的 extractor

axum extractor 只看得到 request：handler 的 state 是 `()`，所以它拿不到 provider。需要依賴時實作 `Extractor`：它是 provider，看得到 request，handler 用 `Extracted<E>` 參數取得結果。

```rust
use hornero::axum::http::request::Parts;
use hornero::prelude::*;

#[derive(Clone)]
struct User {
    name: String,
}

/// 假裝是資料庫
struct Users;

impl Users {
    async fn by_token(&self, token: &str) -> Option<User> {
        (token == "ada-token").then(|| User { name: "ada".into() })
    }
}

/// 依 token 載入目前的使用者
#[derive(Injectable)]
struct CurrentUser {
    users: Arc<Users>,
}

impl Extractor for CurrentUser {
    type Output = User;

    async fn extract(&self, parts: &mut Parts) -> Result<User, HttpError> {
        let token = parts
            .headers
            .get("x-token")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| HttpError::unauthorized("missing token"))?;
        self.users
            .by_token(token)
            .await
            .ok_or_else(|| HttpError::unauthorized("invalid token"))
    }
}

#[controller("/me")]
struct MeController;

#[routes]
impl MeController {
    #[get("/")]
    fn me(&self, Extracted(user): Extracted<CurrentUser>) -> String {
        user.name
    }

    /// 沒有這個參數，所以不會查資料庫
    #[get("/ping")]
    fn ping(&self) -> &'static str {
        "pong"
    }
}

#[module(controllers = [MeController], providers = [value(Users)])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let client = app.client();
assert_eq!(client.get("/me").header("x-token", "ada-token").await.text(), "ada");
assert_eq!(client.get("/me").await.status(), StatusCode::UNAUTHORIZED);
assert_eq!(client.get("/me/ping").await.text(), "pong");
# }
```

幾個重點：

- **參數本身就是宣告**：路由從 handler 的簽名得知 `CurrentUser`，和 `#[guards]` 一樣在 controller 的模組裡解析：看得到 provider 就用那個實例，否則建立一個私有的。依賴缺少是 bootstrap 錯誤，不會等到 request。宣告來自型別而不是 macro，所以 `type Me = Extracted<CurrentUser>` 這種 alias 與手寫路由一樣會被驗證。
- **只有要求它的 handler 才執行**：上例的 `/me/ping` 不會查資料庫。放在 guard 裡的話，每個經過 guard 的 request 都會查。
- **在 guard 與 interceptor 之後執行**：看得到它們寫進 extensions 的資料。回傳的 `Err` 和 handler 的錯誤一樣：用 `?` 拋出的 domain error 交給 exception filter，最後由 renderer 渲染。對 axum extractor 的 rejection 用 `?` 會保留它的狀態碼，例如 `Query::<Page>::from_request_parts(parts, &()).await?` 解析失敗是 400。
- **不讀 body**：後面的參數仍然可以是 `Json<T>`。
- **只能用在 controller handler**：`Extracted<E>` 不是 axum extractor，所以不能放進 mount 的 axum router，也不能包在 `Option` 裡。資料可有可無時，把 `Output` 設成 `Option<User>`。

需要 state 的 axum extractor 不能直接當參數，把它包進 `Extractor` 就能用，例如 axum-extra 的 `PrivateCookieJar` 可以注入 `Key`，再呼叫 `PrivateCookieJar::from_headers(&parts.headers, key)`。

### 同一個 request 共用的資源

`Extractor` 每個參數執行一次，實例也由所有 request 共用，所以不能把 request 的狀態存在 `self`。要在同一個 request 內重複使用結果，把它放進 extensions；要建立在另一個 extractor 之上，就直接注入它：

```rust
use hornero::axum::http::request::Parts;
use hornero::prelude::*;

#[derive(Clone)]
struct User {
    tenant: u64,
}

/// 假裝是資料庫
struct Users;

impl Users {
    async fn load(&self) -> User {
        User { tenant: 7 }
    }
}

#[derive(Injectable)]
struct CurrentUser {
    users: Arc<Users>,
}

impl Extractor for CurrentUser {
    type Output = User;

    async fn extract(&self, parts: &mut Parts) -> Result<User, HttpError> {
        if let Some(user) = parts.extensions.get::<User>() {
            return Ok(user.clone());
        }
        let user = self.users.load().await;
        parts.extensions.insert(user.clone());
        Ok(user)
    }
}

/// 它依賴 CurrentUser，所以 CurrentUser 要註冊在 providers 裡
#[derive(Injectable)]
struct CurrentTenant {
    user: Arc<CurrentUser>,
}

impl Extractor for CurrentTenant {
    type Output = u64;

    async fn extract(&self, parts: &mut Parts) -> Result<u64, HttpError> {
        Ok(self.user.extract(parts).await?.tenant)
    }
}

#[controller("/report")]
struct ReportController;

#[routes]
impl ReportController {
    /// 兩個參數只查一次資料庫
    #[get("/")]
    fn report(
        &self,
        Extracted(user): Extracted<CurrentUser>,
        Extracted(tenant): Extracted<CurrentTenant>,
    ) -> String {
        format!("{} {tenant}", user.tenant)
    }
}

#[module(controllers = [ReportController], providers = [value(Users), CurrentUser])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
assert_eq!(app.client().get("/report").await.text(), "7 7");
# }
```

extensions 裡的值必須是 `Clone`。交易這類不能複製的資源，讓 `Output` 就是它本身：handler 取得所有權，用 `&mut` 傳給 service，最後自己 commit。

### 依 request 資料呈現錯誤

exception filter 與 renderer 只看得到錯誤與路由：它們執行時 request 已經被 handler 消耗了。回應要依 request 的內容變化時，改用手上還有 request 的 middleware 或 interceptor，在呼叫 `next` 之前讀好需要的資料。middleware 之後用 `HttpError::in_response` 取得回應背後的錯誤；interceptor 收到的就是錯誤本身。

```rust
use hornero::axum::body::Body;
use hornero::axum::extract::Request;
use hornero::axum::http::HeaderValue;
use hornero::axum::http::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE};
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
            let page = format!("<h1>{}</h1>", escape_html(err.message()));
            let headers = response.headers_mut();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
            headers.remove(CONTENT_LENGTH); // 換了 body，原本的長度就不對了
            *response.body_mut() = Body::from(page);
        }
        response
    }
}

/// 訊息可能含有 request 的內容（例如 axum 無法解析的路徑參數），放進 HTML 前要 escape
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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

// 路徑參數原樣出現在 axum 的錯誤訊息裡，escape 之後只是文字
let page = client.get("/orders/%3Cscript%3E").header("accept", "text/html").await;
assert_eq!(page.status(), StatusCode::BAD_REQUEST);
assert!(page.text().contains("&lt;script&gt;"), "{}", page.text());

let problem: serde_json::Value = client.get("/orders/7").await.json();
assert_eq!(problem["detail"], "order 7 not found");
# }
```

| 用 | 看得到 | 看不到 |
|---|---|---|
| interceptor | guard 寫入的資料（例如已驗證的使用者）；錯誤本身，可以用 `map_err` 改寫 | guard 的拒絕：被擋下的 request 不會經過 interceptor |
| middleware | 所有錯誤，包括 guard 的 401 / 403 與 axum extractor 的 rejection | guard 寫入的資料：request 在 middleware 交出去之後才經過 guard |

像上例這樣原地修改 response，`HttpError` 會留在 response 上，5xx 的 log 照常記錄。回傳全新的 response 則會連 `HttpError` 一起丟掉，這筆 5xx 就不會被 hornero 記錄。interceptor 改寫後回傳的 `Err` 仍由 renderer 渲染，所以它適合改訊息或加 header；要依 request 換掉整個格式，用上面的 middleware。

有一種情況兩邊都不夠：前一個 guard 寫入了使用者，後一個 guard 拒絕 request，而回應又要用到使用者。interceptor 看不到這個拒絕，middleware 看不到使用者。這時由拒絕的 guard 把需要的資料放進錯誤，例如 `HttpError::forbidden("...").with_details(...)`。

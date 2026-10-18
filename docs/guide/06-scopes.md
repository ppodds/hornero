# 6. Scope：singleton、request、transient

scope 決定一個 provider 實例活多久：

| Scope | 實例數量 | 何時建構 |
|---|---|---|
| `Singleton`（預設） | 整個應用一個 | 啟動時 |
| `Request` | 每個 HTTP request 一個 | 該 request 第一次需要它時（lazy），之後在同一個 request 內共用 |
| `Transient` | 每個使用者（consumer）一個 | 每次有元件依賴它時 |

大多數 provider 都應該是 singleton。request scope 用來承載「這個 request 的」資料，例如目前的使用者、租戶或交易。

## 宣告 scope

```rust
use hornero::prelude::*;
use hornero::ProviderDef;
# struct TraceId(u64);
# async fn make_trace_id() -> Result<TraceId, BoxError> { Ok(TraceId(7)) }

#[derive(Injectable)]
#[inject(scope = request)]
struct AuditLog { /* ... */ }

struct CurrentUser;

#[injectable(scope = request)]
impl CurrentUser {
    fn new(ctx: Arc<RequestContext>) -> Result<Self, BoxError> { /* ... */ Ok(CurrentUser) }
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

## `RequestContext`

`Arc<RequestContext>` 是內建的 request-scoped provider，相當於 Nest 的 `REQUEST`。它是 request head 的快照：

| 方法 | 回傳 |
|---|---|
| `method()` | `&Method` |
| `uri()` | `&Uri` |
| `version()` | `Version` |
| `headers()` | `&HeaderMap` |
| `extensions()` | `&Extensions` |
| `get::<T>()` | `extensions().get::<T>()` 的簡寫 |

每個元件看到的是**它被建構當下**的 request：快照在 request-scoped 的 guard、interceptor 或 handler 建構前建立，並在每個 guard 與 interceptor 之後作廢，下一個需要的元件會重新拍一份；已建構的元件保留自己拿到的快照。因此 **guard 寫進 extensions 的資料，對排在它後面建構的元件（包含 controller 及其依賴）可見**。

## Scope 會傳染

依賴 request-scoped provider 的元件，自己也會變成 request-scoped（和 Nest 相同），一路往上傳到 controller：

```rust
use hornero::prelude::*;

#[derive(Clone)]
struct UserId(String);

#[derive(Injectable)]
struct IdentityGuard;

impl Guard for IdentityGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let user = ctx
            .parts
            .headers
            .get("x-user")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| HttpError::unauthorized("missing x-user header"))?;
        ctx.parts.extensions.insert(UserId(user.to_owned()));
        Ok(())
    }
}

/// 每個 request 一個：目前的使用者
struct CurrentUser {
    id: String,
}

#[injectable(scope = request)]
impl CurrentUser {
    fn new(ctx: Arc<RequestContext>) -> Result<Self, BoxError> {
        let id = ctx.get::<UserId>().ok_or("no user in the request")?;
        Ok(Self { id: id.0.clone() })
    }
}

/// 沒有宣告 scope，但依賴 CurrentUser，所以被傳染成 request-scoped
#[derive(Injectable)]
struct Greeter {
    user: Arc<CurrentUser>,
}

impl Greeter {
    fn greet(&self) -> String {
        format!("hello, {}", self.user.id)
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
    fn greet(&self) -> String {
        self.greeter.greet()
    }
}

#[module(controllers = [GreetController], providers = [CurrentUser, Greeter, IdentityGuard])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
// 不用啟動就能看到哪些東西變成了 request-scoped，以及原因
let report = Hornero::new(AppModule).validate().unwrap();
assert_eq!(
    report.to_string(),
    "  CurrentUser (in AppModule): declared request-scoped
  Greeter (in AppModule): request-scoped via Greeter -> CurrentUser [request]
  GreetController (in AppModule): request-scoped via GreetController -> Greeter -> CurrentUser [request]
"
);

let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let client = app.client();
assert_eq!(client.get("/greet").header("x-user", "ada").await.text(), "hello, ada");
assert_eq!(client.get("/greet").header("x-user", "bob").await.text(), "hello, bob");
# }
```

傳染很方便：深層的 service 可以直接拿到目前的使用者，不必一層層傳參數。但它也是隱性的效能懸崖：一個深層依賴改成 request scope，整條鏈都會在每個 request 重建。所以 hornero 讓傳染**看得見**：

- **scope report**：啟動時用 `tracing::info!` 輸出每個 request-scoped 元件與傳染鏈。
- **可斷言**：`Hornero::validate()` 與 `App::scope_report()` 回傳 `ScopeReport`，可以在測試中檢查，例如 `assert!(report.find("TodoController").is_none())`，避免 controller 意外變成 request-scoped。

## 傳染的限制

以下情況會在啟動時回報 `scope violation`：

**middleware 與 mount 不能變成 request-scoped**：它們在啟動時就要實例化。

```text
[1] scope violation
    middleware ScopedMiddleware (in App) cannot be request-scoped.
    chain: ScopedMiddleware -> RequestId [request]
```

**有生命週期 hook 的 provider 必須是 singleton**：Nest 會靜默地不呼叫 hook，hornero 改為直接報錯。

```text
[1] scope violation
    Hooked (in App) has lifecycle hooks but is request-scoped; hooks only run on singletons.
    chain: Hooked -> AuditLog [request]
```

**明確宣告了 scope 的 provider 不會被傳染**：寫了 `scope = ...` 就是一個承諾。如果它依賴 request-scoped 的東西，啟動會失敗，而不是悄悄改變 scope。這是讓你自選的防護欄：

```rust
use hornero::prelude::*;

#[derive(Injectable)]
#[inject(scope = request)]
struct AuditLog;

#[derive(Injectable)]
#[inject(scope = singleton)] // 承諾：這個 service 永遠是 singleton
struct ReportService {
    audit: Arc<AuditLog>,
}

#[module(providers = [AuditLog, ReportService])]
struct AppModule;

let err = Hornero::new(AppModule).validate().unwrap_err();
assert!(err.to_string().contains(
    "ReportService (in AppModule) is pinned to singleton scope but depends on a request-scoped provider."
));
```

手寫的 provider 用 `ProviderDef::pinned()` 或 `const SCOPE_PINNED: bool = true` 達到相同效果。

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

transient 不會讓依賴它的元件改變 scope；但 transient provider 本身若依賴 request-scoped 的東西，就會被傳染成 request-scoped。

## Runtime 行為與成本

- **純 singleton 的路由零成本**：只有路由鏈上確實有 request-scoped 元件時，hornero 才會為該路由加上 request scope。
- **lazy 建構**：request-scoped 元件在該 request 第一次需要時才建構，之後在同一個 request 內共用。
- **建構失敗會變成 500**：request-scoped 元件在 request 期間建構失敗時，client 會收到不透露細節的 `500 failed to construct a request-scoped component`，真正的原因寫在 log。這個錯誤也會經過 exception filter。
- **request-scoped guard 依賴 `RequestContext` 時**，它拿到的是執行前的快照，看不到排在它後面的 guard 寫入的資料；但這不影響其他元件，它們各自在建構時重新取得快照，所以後面的 guard 與 handler 端的元件仍然看得到。

## 什麼時候不該用 request scope

如果只有 handler 需要 request 資料，**extractor 通常是更好的選擇**：它不會傳染、沒有額外成本，而且型別本身就證明資料存在（例如 `user: AuthUser`）。等到很多深層 service 都需要同一份 request 資料、層層傳參數已經變得痛苦時，再改用 request-scoped provider。

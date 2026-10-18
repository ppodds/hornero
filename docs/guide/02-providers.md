# 2. Provider 與依賴注入

**provider** 是容器負責建構、並以 `Arc<T>` 分享給其他元件的任何東西：service、repository、設定、資料庫連線、HTTP client……。controller、guard、interceptor 也都是 provider。

hornero 的注入 token 就是**型別**：要什麼，就在欄位或參數寫 `Arc<T>`。同一個型別需要多個實例時，再加上一個 qualifier 型別。沒有字串 token，也沒有執行期反射。

## `#[derive(Injectable)]`

最常見的寫法。每個 `Arc<T>` 欄位都是一個依賴；其他欄位必須說明值從哪裡來：

```rust
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use hornero::prelude::*;

#[derive(Injectable)]
struct Clock;

#[derive(Injectable)]
struct SessionStore {
    clock: Arc<Clock>,                     // 依賴：由容器注入
    #[inject(default)]
    sessions: Mutex<HashMap<String, u64>>, // Default::default()
    #[inject(with = Instant::now())]
    started_at: Instant,                   // 任意運算式
}
```

忘了標註非 `Arc` 欄位時會得到編譯錯誤：

```text
error: field `sessions` is not an `Arc<T>`, so it cannot be injected; wrap it in `Arc`, or add `#[inject(default)]` or `#[inject(with = expr)]`
```

struct 層級可以用 `#[inject(...)]` 設定 scope 與生命週期 hook，分別在 [Scope](06-scopes.md) 與 [生命週期](07-lifecycle.md) 介紹：

```rust
use hornero::prelude::*;

#[derive(Injectable)]
#[inject(scope = request, hooks)]
struct AuditLog { /* ... */ }
# impl Lifecycle for AuditLog {}
```

`#[injectable(scope = request)]` 寫在 struct 上，等同於 `#[derive(Injectable)]` 加上 `#[inject(scope = request)]`。

## Constructor injection：`#[injectable]` 寫在 `impl` 上

建構需要運算、驗證或 I/O 時，改用建構函式。`#[injectable]` 會使用 `impl` 區塊裡的 `new`；`new` 可以是 `async`，也可以回傳 `Result<Self, E>`（`E: Into<BoxError>`）。每個參數都必須是 `Arc<T>`（或 `Option<Arc<T>>`，見 [可選的依賴](#可選的依賴)）：

```rust
use hornero::prelude::*;

struct SmtpSettings {
    host: String,
}

struct Mailer {
    host: String,
}

#[injectable]
impl Mailer {
    async fn new(settings: Arc<SmtpSettings>) -> Result<Self, BoxError> {
        if settings.host.is_empty() {
            return Err("smtp host is not configured".into());
        }
        Ok(Self { host: settings.host.clone() })
    }
}

#[module(providers = [value(SmtpSettings { host: String::new() }), Mailer])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
// 建構失敗是 bootstrap 錯誤：應用不會在半殘的狀態下啟動。
let err = Hornero::new(AppModule).build().await.err().unwrap();
assert!(err.to_string().contains("Mailer (in AppModule) failed to construct: smtp host is not configured"));
# }
```

建構函式不叫 `new` 時，用 `#[constructor]` 標出要用的那一個；同一個 `impl` 裡的其他函式不受影響：

```rust
use hornero::prelude::*;

struct Settings;

struct Mailer;

#[injectable]
impl Mailer {
    #[constructor]
    fn from_settings(_settings: Arc<Settings>) -> Self {
        Mailer
    }

    fn new() -> Self {
        Mailer
    }
}
```

**該用哪一種？** 欄位全部是依賴、或只需要預設值時，用 `#[derive(Injectable)]`。需要計算、檢查設定、連線，或欄位型別和依賴型別不同時，用 constructor injection。

## 在模組中註冊 provider

provider 寫進模組的 `providers` 之後才會存在。`providers` 接受五種寫法：

| 寫法 | 提供的型別 | NestJS 對應 |
|---|---|---|
| `T` | `T`，透過 `Injectable` 建構 | class provider |
| `value(expr)` | `expr` 的型別 | `useValue` |
| `factory(closure)` | closure 回傳的 `Ok` 型別 | `useFactory` |
| `bind!(dyn Trait => Impl)` | `dyn Trait`，由 `Impl` 實作 | `{ provide: TOKEN, useClass }` |
| `provider(expr)` | 任何 `ProviderDef`（例如自訂 scope 的 factory） | 自訂 provider |

```rust
use hornero::prelude::*;

trait Notifier: Send + Sync {
    fn notify(&self, message: &str) -> String;
}

#[derive(Injectable)]
struct EmailNotifier;

impl Notifier for EmailNotifier {
    fn notify(&self, message: &str) -> String {
        format!("email: {message}")
    }
}

struct AppConfig {
    greeting: String,
}

struct Greeting(String);

#[derive(Injectable)]
struct WelcomeService {
    notifier: Arc<dyn Notifier>,
    greeting: Arc<Greeting>,
}

impl WelcomeService {
    fn welcome(&self, name: &str) -> String {
        self.notifier.notify(&format!("{}, {name}", self.greeting.0))
    }
}

#[module(providers = [
    WelcomeService,
    bind!(dyn Notifier => EmailNotifier),
    value(AppConfig { greeting: "welcome".into() }),
    factory(|config: Arc<AppConfig>| async move {
        Ok::<_, BoxError>(Greeting(config.greeting.clone()))
    }),
])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
// `App::get` 是給測試與啟動程式碼用的 service locator；一般程式碼請宣告依賴。
let service = app.get::<WelcomeService>().unwrap();
assert_eq!(service.welcome("ada"), "email: welcome, ada");
# }
```

說明：

- **factory** 的參數（最多 8 個）可以是 `Arc<T>`、`Qualified<T, Q>`（見 [下一節](#同一個型別要多個實例)），或是它們的 `Option`（見 [可選的依賴](#可選的依賴)），每個參數都是一個依賴。closure 必須是 async，並回傳 `Result<T, E>`，其中 `E: Into<BoxError>`。factory 是做 I/O 的好地方（連線、讀檔），因為失敗會變成清楚的 bootstrap 錯誤。
- **`bind!`** 之所以是 macro：stable Rust 無法在泛型函式裡把 `Arc<Impl>` 轉成 `Arc<dyn Trait>`，所以由 macro 產生這段轉型。實作型別本身不會被註冊，只有 `dyn Trait` 可以注入。
- **`provider(expr)`** 接受任何回傳 `ProviderDef` 的運算式，是 macro 與手寫 API 之間的橋樑，例如 `provider(ProviderDef::factory(make_client).scope(Scope::Transient))`。`ProviderDef` 的完整 API 見 [不用 macro](13-without-macros.md)。

## Trait object 與 async 方法

把依賴放在 trait 後面，可以在測試時替換實作（見 [測試](09-testing.md)）。有兩個限制要注意：

1. trait 必須是 `Send + Sync`（寫成 supertrait），因為實例會被多個 request 同時使用。
2. trait 中的 `async fn` 無法透過 `dyn` 呼叫。要放在 `Arc<dyn Trait>` 後面的 async 方法，請回傳 `hornero::BoxFuture`：

```rust
use hornero::BoxFuture;
use hornero::prelude::*;

trait UserRepo: Send + Sync {
    fn find_name(&self, id: u64) -> BoxFuture<'_, Option<String>>;
}

#[derive(Injectable)]
struct InMemoryUserRepo;

impl UserRepo for InMemoryUserRepo {
    fn find_name(&self, id: u64) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move { (id == 1).then(|| "ada".to_owned()) })
    }
}

#[derive(Injectable)]
struct UserService {
    repo: Arc<dyn UserRepo>,
}

impl UserService {
    async fn name_of(&self, id: u64) -> Result<String, HttpError> {
        self.repo
            .find_name(id)
            .await
            .ok_or_else(|| HttpError::not_found(format!("user {id} not found")))
    }
}

#[module(providers = [UserService, bind!(dyn UserRepo => InMemoryUserRepo)])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
let users = app.get::<UserService>().unwrap();
assert_eq!(users.name_of(1).await.unwrap(), "ada");
assert_eq!(users.name_of(2).await.unwrap_err().status(), StatusCode::NOT_FOUND);
# }
```

具體型別（例如上例的 `UserService`）不需要 trait。只在真的需要替換的邊界（資料庫、外部服務、時間）加 trait，其他地方直接注入具體型別即可。

## 同一個型別要多個實例

型別就是 token，所以同一個模組不能註冊兩次相同的型別（會得到 `duplicate provider` 錯誤）。需要多個實例時，給每個實例一個 **qualifier**：一個只用來命名的 marker 型別，相當於 Nest 的 `@Inject('TOKEN')`。

| 位置 | 寫法 |
|---|---|
| 註冊 | 在 `value(..)`、`factory(..)`、`bind!(..)` 或 `provider(..)` 後面接 `.qualified::<Q>()`；class provider 寫 `Qualified<T, Q>` |
| export | `exports = [Qualified<T, Q>]` |
| 欄位、建構函式參數 | 型別維持 `Arc<T>`，加上 `#[inject(qualifier = Q)]` |
| factory 參數 | `Qualified<T, Q>`，它 deref 成 `T`，`into_inner()` 取出 `Arc<T>` |
| 測試、啟動程式碼 | `app.get_qualified::<T, Q>()` |

```rust
use hornero::prelude::*;

struct Pool {
    url: &'static str,
}

struct Primary;
struct Replica;

#[derive(Injectable)]
struct ReportService {
    #[inject(qualifier = Primary)]
    writer: Arc<Pool>,
    #[inject(qualifier = Replica)]
    reader: Arc<Pool>,
}

struct HealthCheck(String);

#[module(
    providers = [
        value(Pool { url: "postgres://primary" }).qualified::<Primary>(),
        value(Pool { url: "postgres://replica" }).qualified::<Replica>(),
        factory(|replica: Qualified<Pool, Replica>| async move {
            Ok::<_, BoxError>(HealthCheck(format!("ping {}", replica.url)))
        }),
    ],
    exports = [Qualified<Pool, Primary>, Qualified<Pool, Replica>],
)]
struct DatabaseModule;

#[module(imports = [DatabaseModule], providers = [ReportService])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
let reports = app.get::<ReportService>().unwrap();
assert_eq!((reports.writer.url, reports.reader.url), ("postgres://primary", "postgres://replica"));
assert_eq!(app.get::<HealthCheck>().unwrap().0, "ping postgres://replica");
assert_eq!(app.get_qualified::<Pool, Primary>().unwrap().url, "postgres://primary");
# }
```

規則：

- **qualifier 是型別**：打錯字是編譯錯誤，而不是執行期找不到 provider。qualifier 型別本身不會被建構，空的 `struct Primary;` 就夠了。
- **有 qualifier 與沒有 qualifier 是不同的 provider**：`Pool`、`Pool@Primary`、`Pool@Replica` 可以同時存在，彼此不會造成歧義。要求 `Pool` 時不會「退而求其次」拿到 `Pool@Primary`。錯誤訊息以 `Pool@Primary` 表示帶 qualifier 的 provider，找不到時會列出同一型別有哪些 qualifier。
- **同一個 qualifier 不能註冊兩次**：同一模組裡兩個 `Pool@Primary` 一樣是 `duplicate provider`。

qualifier 以外的做法也還能用：

- **newtype**：`struct PrimaryDb(Pool);`。兩個實例的型別真的不同時（例如各有不同的方法），newtype 比 qualifier 清楚。
- **泛型 tag**：`Db<Primary>`、`Db<Replica>`。整合 crate 想讓整個模組（`SeaOrmModule<Tag>`）都帶著 tag 時適用；`hornero-sea-orm` 就是這樣做的，見 [資料庫](12-database.md#多個資料庫)。

不同模組各自私有地提供同一型別則沒有問題，詳見 [模組](03-modules.md)。

## 可選的依賴

依賴寫成 `Option<Arc<T>>`，就是 Nest 的 `@Optional()`：看得到 `T` 的 provider 時注入它，完全看不到時是 `None`，而不是 `missing dependency` 錯誤。欄位、建構函式參數與 factory 參數都可以這樣寫，也可以搭配 qualifier（`#[inject(qualifier = Q)]` 加 `Option<Arc<T>>`，或 factory 參數 `Option<Qualified<T, Q>>`）。

```rust
use hornero::prelude::*;

struct Metrics;

impl Metrics {
    fn count(&self, _name: &str) {}
}

#[derive(Injectable)]
struct Checkout {
    metrics: Option<Arc<Metrics>>,
}

impl Checkout {
    fn pay(&self) -> &'static str {
        if let Some(metrics) = &self.metrics {
            metrics.count("checkout.pay");
        }
        "paid"
    }
}

#[module(providers = [Checkout])]
struct WithoutMetrics;

#[module(providers = [Checkout, value(Metrics)])]
struct WithMetrics;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(WithoutMetrics).build().await.unwrap();
assert!(app.get::<Checkout>().unwrap().metrics.is_none());

let app = Hornero::new(WithMetrics).build().await.unwrap();
assert!(app.get::<Checkout>().unwrap().metrics.is_some());
assert_eq!(app.get::<Checkout>().unwrap().pay(), "paid");
# }
```

「看得到」與一般依賴的 [可見性規則](03-modules.md) 相同：其他模組私有、或沒有 export 的 provider 也算看不到，所以結果會是 `None`。啟動時，每個拿到 `None` 的可選依賴都會以 debug 等級記錄下來。兩個以上的來源（歧義）仍然是錯誤，可選並不代表「隨便選一個」。

## 內建 provider

以下 provider 在每個模組都可見，不需要註冊：

| 型別 | Scope | 用途 |
|---|---|---|
| `Arc<Shutdown>` | singleton | 得知應用正在關閉、或主動觸發關閉，見 [生命週期](07-lifecycle.md) |
| `Arc<RequestContext>` | request | 目前 request 的 method、URI、headers 與 extensions，見 [Scope](06-scopes.md) |
| `Arc<Routes>` | singleton | 應用所有 controller 路由的清單（method、完整路徑、guard、metadata），產生 API 文件用，見 [OpenAPI](16-openapi.md) |

## 設計規則

- **依賴在型別上宣告**：provider 無法在執行期向容器「拿」東西。依賴寫在型別上，啟動時才能驗證整張圖。可選的依賴也一樣，是否注入在啟動時就決定了。
- **實例是共用的 `Arc<T>`**：同一個實例會同時服務多個 request。需要可變狀態時，使用 `Mutex`、`RwLock`、atomic 等 interior mutability。持有鎖跨越 `.await` 時，請用 `tokio::sync::Mutex`。
- **沒有 `forwardRef`**：循環依賴是 bootstrap 錯誤。把雙方共用的邏輯抽成第三個 provider，或改用 channel 或事件解耦。
- **`Drop` 不能 await**：需要 I/O 的清理（關閉連線池、flush buffer）請放在 [生命週期 hook](07-lifecycle.md)。

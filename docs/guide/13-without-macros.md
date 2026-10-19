# 13. 不用 macro：漸進式採用與自製整合 crate

hornero 的每個 macro 都只是展開成公開的 builder API。所以你可以：

| 層級 | 做法 | 適合 |
|---|---|---|
| **L0** | 把既有的 axum `Router` 掛進模組 | 從 axum 專案逐步遷移 |
| **L1** | 手寫 `impl Module`、`impl Injectable`、`impl Controller` | 不想用 macro、需要動態行為、撰寫整合 crate |
| **L2** | `#[module]`、`#[derive(Injectable)]`、`#[controller]`、`#[routes]` | 日常開發 |
| **L3** | 整合 crate（`hornero-config`、`hornero-sea-orm`……） | 基礎設施 |

各層可以在同一個應用裡混用。

## L0：掛進既有的 axum Router

已經寫好的 axum router 可以原封不動地掛進模組。需要依賴時，用 `mount` 並在 factory 裡取得它們（例如交給 axum 的 `State`）：

```rust
use std::sync::atomic::{AtomicU64, Ordering};

use hornero::axum::Router;
use hornero::axum::extract::State;
use hornero::axum::routing::get;
use hornero::prelude::*;

struct HitCounter(AtomicU64);

/// 既有的 axum 程式碼：完全不知道 hornero 的存在
fn legacy_router() -> Router<Arc<HitCounter>> {
    Router::new().route(
        "/hits",
        get(|State(counter): State<Arc<HitCounter>>| async move {
            (counter.0.fetch_add(1, Ordering::Relaxed) + 1).to_string()
        }),
    )
}

struct LegacyModule;

impl Module for LegacyModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide_value(HitCounter(AtomicU64::new(0)))
            // 沒有依賴的 router
            .mount_router("/health", Router::new().route("/", get(|| async { "ok" })))
            // 由注入的 provider 組出 router
            .mount("/legacy", |counter: Arc<HitCounter>| async move {
                Ok::<_, BoxError>(legacy_router().with_state(counter))
            });
    }
}

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(LegacyModule).build().await.unwrap();
let client = app.client();
assert_eq!(client.get("/health").await.text(), "ok");
assert_eq!(client.get("/legacy/hits").await.text(), "1");
assert_eq!(client.get("/legacy/hits").await.text(), "2");
# }
```

- mount 的路徑會加上 global prefix；模組的 middleware 與 layer 也會包住它。
- mount 進來的 router 不經過 guard、interceptor 與 filter（它們是 controller 路由的功能）。要保護它，請在 axum router 上加 layer，或放進有 middleware 的模組。
- `mount` 的 factory 必須是 singleton（它在啟動時執行），依賴 request-scoped 的東西會是 bootstrap 錯誤。

反過來，把 hornero 應用嵌入既有的 axum 應用也可以，見 [常見做法](14-recipes.md#嵌入既有的-axum-應用)。

## L1：手寫 trait

### `Injectable`

```rust
use hornero::{BoxError, Injectable, Key, Resolver};
use hornero::prelude::*;

struct Clock;

struct Greeter {
    clock: Arc<Clock>,
    greeting: String, // 不是依賴的欄位，自己決定怎麼初始化
}

impl Injectable for Greeter {
    // 以下三個常數都有預設值，需要時再覆寫：
    // const SCOPE: Scope = Scope::Request;
    // const SCOPE_PINNED: bool = true;
    // const LIFECYCLE: Option<LifecycleFn<Self>> = hornero::hooks::<Self>();

    fn dependencies() -> Vec<Key> {
        vec![Key::of::<Clock>()]
    }

    async fn construct(resolver: &Resolver) -> Result<Self, BoxError> {
        Ok(Greeter { clock: resolver.get::<Clock>()?, greeting: "hello".into() })
    }
}

#[module(providers = [value(Clock), Greeter])]
struct AppModule;

Hornero::new(AppModule).validate().unwrap();
```

`Resolver` 只裝著 `dependencies()` 宣告過的依賴。向它要沒宣告的型別會得到錯誤，而不是偷偷從容器裡拿：依賴圖之所以能在啟動時完整驗證，靠的就是這條規則。

帶 qualifier 與可選的依賴，在 `dependencies()` 裡用 `Key` 的 builder 宣告，再用 `resolver.resolve::<D>()` 取出，`D` 是對應的 [`Dependency`](../api/hornero-core.md#trait-dependency) 型別：

```rust
use hornero::{BoxError, Injectable, Key, Resolver};
use hornero::prelude::*;

struct Pool;
struct Metrics;
struct Primary;

struct Reports {
    pool: Arc<Pool>,
    metrics: Option<Arc<Metrics>>,
}

impl Injectable for Reports {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<Pool>().qualified::<Primary>(), Key::of::<Metrics>().optional()]
    }

    async fn construct(resolver: &Resolver) -> Result<Self, BoxError> {
        Ok(Reports {
            pool: resolver.resolve::<Qualified<Pool, Primary>>()?.into_inner(),
            metrics: resolver.resolve()?,
        })
    }
}

struct AppModule;

impl Module for AppModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provider(hornero::ProviderDef::value(Pool).qualified::<Primary>())
            .provide::<Reports>()
            .export_qualified::<Pool, Primary>();
    }
}

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
assert!(app.get::<Reports>().unwrap().metrics.is_none());
# }
```

### `Controller`

```rust
use hornero::axum::http::Method;
use hornero::{BoxError, Controller, ControllerDef, Injectable, Key, Resolver};
use hornero::prelude::*;

struct Admin;

#[derive(Injectable)]
struct AdminGuard;

impl Guard for AdminGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        match ctx.route.meta::<Admin>() {
            Some(_) if ctx.parts.headers.contains_key("x-admin") => Ok(()),
            Some(_) => Err(HttpError::forbidden("administrators only")),
            None => Ok(()),
        }
    }
}

struct UserController;

impl Injectable for UserController {
    fn dependencies() -> Vec<Key> {
        Vec::new()
    }

    async fn construct(_: &Resolver) -> Result<Self, BoxError> {
        Ok(UserController)
    }
}

impl UserController {
    async fn find_one(&self, id: u64) -> Result<String, HttpError> {
        Ok(format!("user {id}"))
    }
}

impl Controller for UserController {
    fn routes(c: &mut ControllerDef<Self>) {
        c.prefix("/users").guard::<AdminGuard>();

        // handler 是 closure：第一個參數是 Arc<Self>，其餘是 axum extractor
        c.get("/{id}", "find_one", |this: Arc<Self>, Path(id): Path<u64>| async move {
            this.find_one(id).await
        });

        c.delete("/{id}", "remove", |_this: Arc<Self>, Path(id): Path<u64>| async move {
            format!("removed {id}")
        })
        .meta(Admin);

        c.route(Method::OPTIONS, "/", "options", |_this: Arc<Self>| async { StatusCode::NO_CONTENT });
    }
}

struct AppModule;

impl Module for AppModule {
    fn define(&self, m: &mut ModuleDef) {
        m.controller::<UserController>();
    }
}

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let client = app.client();
assert_eq!(client.get("/users/7").await.text(), "user 7");
assert_eq!(client.delete("/users/7").await.status(), StatusCode::FORBIDDEN);
assert_eq!(client.delete("/users/7").header("x-admin", "1").await.text(), "removed 7");
# }
```

handler 的名稱（第二個參數）會出現在 `RouteContext::handler()`、log 與錯誤訊息中。

### `Module`

```rust
use std::time::Duration;

use hornero::prelude::*;
use tower_http::timeout::TimeoutLayer;
# use hornero::axum::extract::Request;
# use hornero::axum::middleware::Next;
# struct DatabaseModule;
# impl Module for DatabaseModule {
#     fn define(&self, _: &mut ModuleDef) {}
# }
# struct Db;
# struct Stats;
# impl Stats {
#     async fn load(_: &Db) -> Result<Stats, BoxError> { Ok(Stats) }
# }
# struct PageSize(usize);
# #[derive(Injectable)]
# struct UserService;
# trait UserRepo: Send + Sync {}
# #[derive(Injectable)]
# struct SqlUserRepo;
# impl UserRepo for SqlUserRepo {}
# #[derive(Injectable)]
# struct RequestId;
# impl Middleware for RequestId {
#     async fn handle(&self, req: Request, next: Next) -> Response { next.run(req).await }
# }
# #[controller("/users")]
# struct UserController;
# #[routes]
# impl UserController {}
# struct UsersModule;

impl Module for UsersModule {
    fn define(&self, m: &mut ModuleDef) {
        m.import(DatabaseModule)
            .provide::<UserService>()
            .provider(hornero::bind!(dyn UserRepo => SqlUserRepo))
            .provide_value(PageSize(20))
            .factory(|db: Arc<Db>| async move { Ok::<_, BoxError>(Stats::load(&db).await?) })
            .controller::<UserController>()
            .middleware::<RequestId>()
            .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(5)))
            .export::<UserService>();
    }
}
```

## Macro 與 L1 的對應

| Macro | 展開成 |
|---|---|
| `#[derive(Injectable)]` | `impl Injectable`：每個 `Arc<T>` 欄位對應一個 `Key::of::<T>()` 與一次 `resolver.get::<T>()`；`#[inject(qualifier = Q)]` 與 `Option<Arc<T>>` 欄位對應 `Key::of::<T>().qualified::<Q>()`、`.optional()` 與 `resolver.resolve::<..>()` |
| `#[injectable] impl` | `impl Injectable`：建構函式的每個參數對應一個依賴，最後呼叫建構函式 |
| `#[module(...)]` | `impl Module`：每個選項對應一次 `m.global()`、`m.import(..)`、`m.provide::<T>()`、`m.provider(..)`、`m.controller::<C>()`、`m.middleware::<M>()`、`m.export::<T>()`、`m.export_qualified::<T, Q>()`、`m.export_module::<M>()`；`value(..)` 與 `factory(..)` 變成 `m.provider(ProviderDef::value(..))` 與 `m.provider(ProviderDef::factory(..))` |
| `#[controller(...)]` | `#[derive(Injectable)]` 加上 `impl ControllerAttrs`，在其中設定前綴、enhancer 與 metadata |
| `#[routes]` | `impl Controller`：先呼叫 `ControllerAttrs::configure`，再為每個方法呼叫一次 `c.route(..)` |

想看展開結果，可以用 `cargo expand`。

## `ProviderDef`

每種 provider 寫法最後都會變成一個 `ProviderDef`。它也可以透過 `#[module(providers = [provider(...)])]` 或 `m.provider(...)` 直接註冊：

| 建構方式 | 用途 |
|---|---|
| `ProviderDef::injectable::<T>()` | class provider（`m.provide::<T>()` 的底層） |
| `ProviderDef::bind::<dyn Trait, Impl>(coerce)` | trait binding（`bind!` 的底層） |
| `ProviderDef::value(v)` / `ProviderDef::from_arc(arc)` | 固定值；`from_arc` 可以接受 `Arc<dyn Trait>` |
| `ProviderDef::factory(closure)` | async factory，參數是注入的 `Arc<T>`、`Qualified<T, Q>` 或它們的 `Option` |
| `ProviderDef::from_fn::<T, _, _>(deps, \|resolver\| ...)` | 最底層：自己列出依賴，自己從 `Resolver` 取值 |

| Builder | 用途 |
|---|---|
| `.scope(Scope::Request)` | 設定 scope |
| `.pinned()` | 禁止被傳染成 request scope |
| `.qualified::<Q>()` | 以 qualifier `Q` 註冊，見 [同一個型別要多個實例](02-providers.md#同一個型別要多個實例) |
| `.with_hooks::<T>()` | 為 factory 或 value provider 啟用 `T` 的 `Lifecycle` hook |

`from_fn` 可以做出 Nest 的 `useExisting`，讓 trait 指向一個已經存在的實例：

```rust
use hornero::{Key, ProviderDef};
use hornero::prelude::*;

trait Notifier: Send + Sync {}

#[derive(Injectable)]
struct EmailNotifier;

impl Notifier for EmailNotifier {}

#[module(providers = [
    EmailNotifier,
    // dyn Notifier 是既有 EmailNotifier 實例的別名，不會另外建構一個
    provider(ProviderDef::from_fn::<dyn Notifier, _, _>(vec![Key::of::<EmailNotifier>()], |resolver| async move {
        let email: Arc<EmailNotifier> = resolver.get()?;
        Ok(email as Arc<dyn Notifier>)
    })),
])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
let email = app.get::<EmailNotifier>().unwrap();
let notifier = app.get::<dyn Notifier>().unwrap();
assert_eq!(Arc::as_ptr(&email) as *const (), Arc::as_ptr(&notifier) as *const ());
# }
```

## 自製整合 crate

`hornero-config` 與 `hornero-sea-orm` 都是一般的 crate，沒有用到任何內部 API。自己寫一個整合 crate 的模式如下：

```rust
use hornero::prelude::*;
use hornero::{BoxFuture, FactoryFn, Key, ProviderDef, Resolver};

// ---- 整合 crate 的公開 API ----

pub struct MailerOptions {
    pub smtp_url: String,
}

/// 這個 crate 提供的 provider
pub struct Mailer {
    smtp_url: String,
}

impl Mailer {
    async fn connect(options: MailerOptions) -> Result<Self, BoxError> {
        Ok(Mailer { smtp_url: options.smtp_url }) // 真正的實作會在這裡連線
    }

    pub fn smtp_url(&self) -> &str {
        &self.smtp_url
    }
}

impl Lifecycle for Mailer {
    async fn on_application_shutdown(&self) -> Result<(), BoxError> {
        Ok(()) // 送出佇列中的信件、關閉連線
    }
}

type OptionsFactory =
    Arc<dyn Fn(&Resolver) -> BoxFuture<'static, Result<MailerOptions, BoxError>> + Send + Sync>;

pub struct MailerModule {
    deps: Vec<Key>,
    options: OptionsFactory,
    global: bool,
}

impl MailerModule {
    /// 由注入的 provider 計算設定（Nest 的 forRootAsync）
    pub fn for_root_with<Args, F>(factory: F) -> Self
    where
        F: FactoryFn<Args, Output = MailerOptions>,
    {
        let factory = Arc::new(factory);
        MailerModule {
            deps: F::dependencies(),
            options: Arc::new(move |resolver| factory.call(resolver)),
            global: true,
        }
    }

    /// 只讓 import 它的模組看到 Mailer
    pub fn local(mut self) -> Self {
        self.global = false;
        self
    }
}

impl Module for MailerModule {
    fn define(&self, m: &mut ModuleDef) {
        if self.global {
            m.global();
        }
        let options = self.options.clone();
        m.provider(
            ProviderDef::from_fn::<Mailer, _, _>(self.deps.clone(), move |resolver| {
                let options = options(&resolver);
                async move { Ok(Arc::new(Mailer::connect(options.await?).await?)) }
            })
            .with_hooks::<Mailer>(),
        )
        .export::<Mailer>();
    }
}

// ---- 使用者的應用 ----

struct SmtpUrl(String);

#[module(global, providers = [value(SmtpUrl("smtp://localhost:25".into()))], exports = [SmtpUrl])]
struct SettingsModule;

#[module(imports = [
    SettingsModule,
    MailerModule::for_root_with(|url: Arc<SmtpUrl>| async move {
        Ok::<_, BoxError>(MailerOptions { smtp_url: url.0.clone() })
    }),
])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
assert_eq!(app.get::<Mailer>().unwrap().smtp_url(), "smtp://localhost:25");
app.close().await.unwrap();
# }
```

撰寫整合 crate 的檢查清單：

- **依賴 `hornero` facade**。hornero 的 macro 產生的是 `::hornero::...` 路徑，所以使用 macro 的 crate 必須以 `hornero` 這個名稱依賴它。
- **`define` 只負責描述**：它可能被呼叫多次（`validate` 與 `build`）。連線、讀檔放在 factory 裡，失敗時會變成清楚的 bootstrap 錯誤。
- **基礎設施模組預設 global**，並提供 `.local()` 讓使用者關閉。
- **只 export 使用者需要的型別**，內部 helper 保持私有。
- **清理用 `Lifecycle`**，並以 `.with_hooks::<T>()` 啟用。
- **宣告模組的身分**：`for_root` 這類每次呼叫都該是獨立模組的，在 `fn identity()` 回傳 `Identity::Instance`（不需要自己產生 id）；內容相同的 import 應該合併時，回傳 `Identity::key(..)`。帶資料卻沒宣告的模組被 import 兩次會是 bootstrap 錯誤。
- **允許多個實例**：讓 provider 帶泛型 tag（`Db<Tag>`、`SeaOrmModule<Tag>`），多個實例才不會 export 同一個型別而造成歧義。
- **用 `hornero-testing` 測試**你的模組：`TestApp::new(MyModule::for_root(...))`。

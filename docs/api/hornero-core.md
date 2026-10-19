# `hornero-core` API 參考

`hornero-core` 是 hornero 的核心：DI 容器、模組系統、HTTP pipeline、生命週期與錯誤型別。應用程式通常不直接依賴它，而是透過 [`hornero`](hornero.md) facade 使用，所有項目都以 `hornero::` 開頭 re-export。

| 分類 | 項目 |
|---|---|
| [應用程式](#應用程式) | `Hornero`、`App` |
| [模組](#模組) | `Module`、`ModuleDef`、`Provide` |
| [Provider](#provider) | `Injectable`、`Scope`、`hooks`、`LifecycleFn`、`ProviderDef`、`FactoryFn`、`bind!`、`Key`、`Dependency`、`Qualified`、`Resolver` |
| [Request scope](#request-scope) | `RequestContext`、`ScopeReport`、`ScopeEntry` |
| [HTTP](#http) | `Controller`、`ControllerAttrs`、`ControllerDef`、`RouteDef`、`RouteHandler`、`Guard`、`GuardContext`、`Interceptor`、`ExceptionFilter`、`AppGuard`、`AppInterceptor`、`AppFilter`、`Middleware`、`RouteContext`、`Metadata`、`Routes`、`RouteInfo`、`EnhancerInfo` |
| [錯誤](#錯誤) | `HttpError`、`BootError`、`ShutdownError`、`Problem` |
| [生命週期](#生命週期) | `Lifecycle`、`Hook`、`Shutdown` |
| [工具型別](#工具型別) | `BoxError`、`BoxFuture` |

---

## 應用程式

### `Hornero`

```rust,ignore
pub struct Hornero { /* ... */ }
```

應用程式的 builder：根模組，加上全域 enhancer、layer 與 override。所有設定方法都取得並回傳 `self`。

| 方法 | 說明 |
|---|---|
| `fn new(root: impl Module) -> Hornero` | 以根模組建立 builder |
| `fn global_prefix(self, prefix: impl Into<String>) -> Self` | 所有 controller 路由與 mount 的路徑前綴，例如 `/api` |
| `fn shutdown_timeout(self, timeout: Duration) -> Self` | `serve`（與 `listen`）在關機開始後等待進行中 request 的上限，預設 10 秒；逾時或收到第二次 SIGINT/SIGTERM 就放棄剩下的連線，仍會執行關閉 hook。`Duration::MAX` 表示無限等待 |
| `fn guard<G: Guard + Injectable>(self) -> Self` | 全域 guard，在根模組解析；在模組宣告的 [`AppGuard`](#appguardappinterceptorappfilter) 以及 controller、handler 的 guard 之前執行 |
| `fn interceptor<I: Interceptor + Injectable>(self) -> Self` | 全域 interceptor，包住模組宣告的 `AppInterceptor` 以及 controller、handler 的 interceptor |
| `fn filter<F: ExceptionFilter + Injectable>(self) -> Self` | 全域 exception filter；在 handler 與 controller 層級的 filter 都沒有接住錯誤時才比對，比模組宣告的 `AppFilter` 先比對 |
| `fn middleware<M: Middleware + Injectable>(self) -> Self` | 全域 DI middleware，包住所有路由與 404 fallback |
| `fn layer<L>(self, layer: L) -> Self` | 包住整個應用的 tower layer；先加的在外層 |
| `fn override_provider<T: ?Sized>(self, value: Arc<T>) -> Self` | 以 `value` 取代所有 `T` 的 provider；原本的依賴不再需要 |
| `fn override_value<T>(self, value: T) -> Self` | 同上，傳值 |
| `fn override_with(self, def: ProviderDef) -> Self` | 以任意 `ProviderDef` 取代所有 `def.key()` 的 provider；取代帶 qualifier 的 provider 也用它：`override_with(ProviderDef::value(fake).qualified::<Primary>())`。同一個 key 註冊多次時，最後註冊的生效，被取代的不會回報為 `UnusedOverride` |
| `fn override_guard<G>(self, guard: impl Guard) -> Self` | 以任意 `Guard` 取代所有用到 guard `G` 的地方 |
| `fn override_interceptor<I>(self, interceptor: impl Interceptor) -> Self` | 同上，interceptor |
| `fn override_filter<F>(self, filter: impl ExceptionFilter) -> Self` | 同上，exception filter |
| `fn routes(&self) -> Result<Routes, BootError>` | 列出所有 controller 路由（見 [`Routes`](#routesrouteinfo-與-enhancerinfo)），和 `validate` 一樣驗證依賴圖與路由、不建構任何東西；產生 API 文件用 |
| `fn validate(&self) -> Result<ScopeReport, BootError>` | 只驗證依賴圖與路由，不建構任何東西。路由衝突用 dry-run 組裝 router 來檢查（controller 路由用佔位 handler，`mount_router` 用真正的 router），所以 axum 自己會拒絕的衝突（例如 mount 與 controller 路由重疊）在這裡就會找到；需要 DI 的 `mount(..)` factory 只有 `build` 檢查得到 |
| `async fn build(self) -> Result<App, BootError>` | 驗證、建構所有 singleton、組出 router，再執行 `on_module_init` 與 `on_application_bootstrap`。建構之後任何一步失敗，都會觸發 `Shutdown` 並對已建構的有 hook 的 singleton 執行 `on_module_destroy` 與 `on_application_shutdown`（反向依賴順序），清理失敗接在原本的問題之後 |
| `async fn listen(self, addr: impl ToSocketAddrs) -> Result<(), BoxError>` | `build` 後開始 serve，直到收到 SIGINT / SIGTERM 或 `Shutdown::trigger`，最後執行關閉 hook |

`layer` 的完整 bound 與 axum 的 `Router::layer` 相同：`L: Layer<Route> + Clone + Send + Sync + 'static`，且 `L::Service` 是回應為 `IntoResponse`、錯誤為 `Infallible` 的 `Service<Request>`。

```rust
use hornero::prelude::*;
use tower_http::trace::TraceLayer;

#[derive(Injectable)]
struct AllowAll;

impl Guard for AllowAll {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

#[module]
struct AppModule;

let hornero = Hornero::new(AppModule)
    .global_prefix("/api")
    .guard::<AllowAll>()
    .layer(TraceLayer::new_for_http());

let report = hornero.validate().unwrap();
assert!(report.is_empty());
```

### `App`

```rust,ignore
pub struct App { /* ... */ }
```

已啟動的應用：所有 singleton 都已建構，`on_module_init` 與 `on_application_bootstrap` 都已執行。

| 方法 | 說明 |
|---|---|
| `fn router(&self) -> Router` | 組好的 axum `Router`（clone 成本很低） |
| `fn scope_report(&self) -> &ScopeReport` | 哪些元件是 request-scoped，以及原因 |
| `fn routes(&self) -> &Routes` | 所有 controller 路由，與 `Hornero::routes` 相同 |
| `fn shutdown_handle(&self) -> Shutdown` | 應用的 `Shutdown`；呼叫 `trigger()` 會停止 `serve` |
| `fn get<T: ?Sized>(&self) -> Option<Arc<T>>` | 依型別取得 singleton：根模組看得到的那個，或整個應用唯一的那個。service locator，只給測試與啟動程式碼使用 |
| `fn get_qualified<T: ?Sized, Q>(&self) -> Option<Arc<T>>` | 同上，取得以 qualifier `Q` 註冊的 `T` |
| `async fn serve(self, listener: TcpListener) -> Result<(), BoxError>` | 在 `listener` 上 serve，處理 graceful shutdown，最後執行關閉 hook；handler 可以使用 `ConnectInfo<SocketAddr>`；等待進行中 request 的時間受 `Hornero::shutdown_timeout` 限制 |
| `async fn close(self) -> Result<(), ShutdownError>` | 觸發 `Shutdown`，再依反向依賴順序執行 `on_module_destroy` 與 `on_application_shutdown`；即使有 hook 失敗也會全部執行 |

---

## 模組

### `trait Module`

```rust,ignore
pub trait Module: Any + Send + Sync {
    fn define(&self, m: &mut ModuleDef);
    fn dedup_key(&self) -> Option<u64> { None }
    fn name(&self) -> String { /* 型別名稱 */ }
}
```

封裝的單位。`#[module]` 會產生這個 impl。

| 方法 | 說明 |
|---|---|
| `define` | 用 `ModuleDef` 描述模組內容。可能被呼叫多次（`validate` 與 `build`），所以不要做 I/O |
| `dedup_key` | 同型別、key 相同的 import 合併成一個模組。預設 `None`：沒有欄位的模組以型別為身分（import 幾次都是同一個實例），帶欄位的模組每次 import 都是獨立實例 |
| `name` | 錯誤訊息與 scope report 中的名稱，預設是不含路徑的型別名稱 |

見 [模組](../guide/03-modules.md)。

### `ModuleDef`

```rust,ignore
pub struct ModuleDef { /* ... */ }
```

傳給 `Module::define` 的 builder。每個方法都回傳 `&mut Self`，可以串接。

| 方法 | 說明 |
|---|---|
| `fn global(&mut self)` | 讓本模組的 exports 對所有模組可見 |
| `fn import(&mut self, module: impl Module)` | import 一個模組 |
| `fn provide<T: Provide>(&mut self)` | 註冊 class provider（任何 `Injectable` 型別；`Qualified<T, Q>` 以 qualifier `Q` 註冊 `T`），或全域 enhancer token，例如 `AppGuard<G>`；見 [`Provide`](#trait-provide) |
| `fn provide_value<T>(&mut self, value: T)` | 註冊固定值（`useValue`） |
| `fn provide_arc<T: ?Sized>(&mut self, value: Arc<T>)` | 註冊固定的 `Arc`，可以是 `Arc<dyn Trait>` |
| `fn factory<Args, F: FactoryFn<Args>>(&mut self, f: F)` | 註冊 async factory，參數是注入的 [`Dependency`](#trait-dependency)（`Arc<T>`、`Qualified<T, Q>` 或它們的 `Option`） |
| `fn provider(&mut self, def: ProviderDef)` | 註冊任意 `ProviderDef`（例如 `bind!` 的結果） |
| `fn controller<C: Controller>(&mut self)` | 註冊 controller |
| `fn export<T: ?Sized>(&mut self)` | export 一個 provider：本模組提供的，或 import 進來的（轉出） |
| `fn export_qualified<T: ?Sized, Q>(&mut self)` | 同上，export 以 qualifier `Q` 註冊的 `T` |
| `fn export_module<M: Module>(&mut self)` | 轉出型別為 `M` 的 import 的所有 exports，不論是哪個值（`for_root(..)` 也可以）；兩個實例 export 同一個型別時是歧義錯誤 |
| `fn middleware<M: Middleware + Injectable>(&mut self)` | 只包住本模組 controller 與 mount 的 DI middleware |
| `fn layer<L>(&mut self, layer: L)` | 只包住本模組 controller 與 mount 的 tower layer |
| `fn mount_router(&mut self, path: impl Into<String>, router: Router)` | 掛進一個 axum `Router`（L0） |
| `fn mount<Args, F>(&mut self, path: impl Into<String>, f: F)` | 掛進一個由 factory 產生的 `Router`；`F: FactoryFn<Args, Output = Router>` |

```rust
use hornero::prelude::*;

#[derive(Injectable)]
struct UserService;

struct UsersModule;

impl Module for UsersModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide::<UserService>()
            .provide_value(42_u32)
            .export::<UserService>();
    }
}

Hornero::new(UsersModule).validate().unwrap();
```

### `trait Provide`

```rust,ignore
pub trait Provide: 'static {
    fn register(m: &mut ModuleDef);
}

impl<T: Injectable> Provide for T { /* m.provider(ProviderDef::injectable::<T>()) */ }
impl<T: Injectable, Q> Provide for Qualified<T, Q> { /* m.provider(ProviderDef::injectable::<T>().qualified::<Q>()) */ }
impl<G: Guard + Injectable> Provide for AppGuard<G> { /* ... */ }
impl<I: Interceptor + Injectable> Provide for AppInterceptor<I> { /* ... */ }
impl<F: ExceptionFilter + Injectable> Provide for AppFilter<F> { /* ... */ }
```

`ModuleDef::provide::<T>()` 與 `#[module(providers = [T])]` 接受的型別。一般的 `Injectable` 型別註冊成 class provider；[`Qualified<T, Q>`](#qualified) 把 `T` 註冊在 qualifier `Q` 之下；[`AppGuard<G>`、`AppInterceptor<I>`、`AppFilter<F>`](#appguardappinterceptorappfilter) 則註冊全域 enhancer。型別沒有實作 `Injectable` 時，編譯錯誤是「`` `T` is not injectable ``」。

---

## Provider

### `trait Injectable`

```rust,ignore
pub trait Injectable: Send + Sync + Sized + 'static {
    const SCOPE: Scope = Scope::Singleton;
    const SCOPE_PINNED: bool = false;
    const LIFECYCLE: Option<LifecycleFn<Self>> = None;

    fn dependencies() -> Vec<Key>;
    fn construct(resolver: &Resolver) -> impl Future<Output = Result<Self, BoxError>> + Send;
    fn describe(_meta: &mut Metadata) {}
}
```

容器知道如何建構的型別。通常由 `#[derive(Injectable)]` 或 `#[injectable]` 產生。

| 項目 | 說明 |
|---|---|
| `SCOPE` | 宣告的 scope；實際 scope 仍可能因傳染變成 `Request` |
| `SCOPE_PINNED` | `true` 時，被傳染成 request scope 會是 bootstrap 錯誤 |
| `LIFECYCLE` | 生命週期 hook，用 `hornero::hooks::<Self>()` 啟用 |
| `dependencies()` | 依賴的 `Key`，順序與 `construct` 取用的順序一致；帶 qualifier 的依賴寫 `Key::of::<T>().qualified::<Q>()`，可選的依賴加上 `.optional()` |
| `construct(resolver)` | 從 `resolver` 取得依賴並建構實例；可以用 `async fn` 實作 |
| `describe(meta)` | 型別當作 guard、interceptor、filter 或 middleware 使用時描述自己的 metadata，從路由表的 [`EnhancerInfo::meta`](#routesrouteinfo-與-enhancerinfo) 讀回；預設什麼都不放。開啟 `hornero` 的 `openapi` feature 時，`#[derive(Injectable)]` 會替實作 `ApiSecurity` 的 guard 填入 security scheme；手寫的 impl 呼叫 `hornero_openapi::describe_security::<Self>(meta)` |

```rust
use hornero::{BoxError, Injectable, Key, Resolver};
use hornero::prelude::*;

struct Clock;

struct Greeter {
    clock: Arc<Clock>,
}

impl Injectable for Greeter {
    fn dependencies() -> Vec<Key> {
        vec![Key::of::<Clock>()]
    }

    async fn construct(resolver: &Resolver) -> Result<Self, BoxError> {
        Ok(Greeter { clock: resolver.get()? })
    }
}
```

### `enum Scope`

```rust,ignore
pub enum Scope { Singleton, Request, Transient }
```

| 變體 | 說明 |
|---|---|
| `Singleton` | 整個應用一個，啟動時建構。預設值 |
| `Request` | 每個 request 一個，第一次需要時建構；會向上傳染 |
| `Transient` | 每個依賴它的元件各一個 |

實作了 `Clone`、`Copy`、`Debug`、`PartialEq`、`Eq`、`Hash`。見 [Scope](../guide/06-scopes.md)。

### `hooks` 與 `LifecycleFn`

```rust,ignore
pub const fn hooks<T: Lifecycle>() -> Option<LifecycleFn<T>>;
pub type LifecycleFn<T> = fn(&Arc<T>) -> Arc<dyn ErasedLifecycle>;
```

手寫 `Injectable` 時啟用生命週期 hook：`const LIFECYCLE: Option<LifecycleFn<Self>> = hornero::hooks::<Self>();`。`#[inject(hooks)]` 會產生這一行。

### `ProviderDef`

```rust,ignore
pub struct ProviderDef { /* ... */ }
```

type-erased 的 provider 註冊：滿足哪個 key、依賴什麼、如何建構。每種註冊寫法最後都會產生一個 `ProviderDef`。實作了 `Clone`。

| 建構方式 | 說明 |
|---|---|
| `fn injectable<T: Injectable>() -> ProviderDef` | class provider |
| `fn bind<D: ?Sized, I: Injectable>(coerce: fn(Arc<I>) -> Arc<D>) -> ProviderDef` | 建構 `I`，以 `Arc<D>` 提供；`bind!` 會寫好 `coerce` |
| `fn value<T>(value: T) -> ProviderDef` | 固定值 |
| `fn from_arc<T: ?Sized>(value: Arc<T>) -> ProviderDef` | 固定的 `Arc`，可以是 `Arc<dyn Trait>` |
| `fn factory<Args, F: FactoryFn<Args>>(f: F) -> ProviderDef` | async factory，參數是注入的 [`Dependency`](#trait-dependency) |
| `fn from_fn<T: ?Sized, F, Fut>(deps: Vec<Key>, f: F) -> ProviderDef` | 最底層：明確列出依賴，`f: Fn(Resolver) -> Fut`，`Fut: Future<Output = Result<Arc<T>, BoxError>>` |

| Builder 與存取 | 說明 |
|---|---|
| `fn scope(self, scope: Scope) -> Self` | 設定 scope |
| `fn pinned(self) -> Self` | 禁止被傳染成 request scope |
| `fn qualified<Q: 'static>(self) -> Self` | 以 qualifier `Q` 註冊，讓同一個模組可以有多個同型別的 provider；使用方以 `#[inject(qualifier = Q)]` 或 [`Qualified<T, Q>`](#qualified) 指定 |
| `fn with_hooks<T: Lifecycle>(self) -> Self` | 為型別為 `T` 的 factory 或 value provider 啟用 hook；`T` 必須是這個 provider 的型別（qualifier 不影響），否則啟動時回報 `Problem::InvalidProvider` |
| `fn key(&self) -> Key` | 這個 provider 滿足的 key |
| `fn dependencies(&self) -> &[Key]` | 依賴的 key |

```rust
use hornero::{Key, ProviderDef};
use hornero::prelude::*;

struct Config(&'static str);
struct Client(String);

let def = ProviderDef::factory(|config: Arc<Config>| async move {
    Ok::<_, BoxError>(Client(config.0.to_owned()))
})
.scope(Scope::Transient);

assert_eq!(def.key(), Key::of::<Client>());
assert_eq!(def.dependencies(), [Key::of::<Config>()]);

struct Primary;
let primary = ProviderDef::value(Config("primary")).qualified::<Primary>();
assert_eq!(primary.key(), Key::of::<Config>().qualified::<Primary>());
```

### `trait FactoryFn`

```rust,ignore
pub trait FactoryFn<Args>: Send + Sync + 'static {
    type Output: Send + Sync + 'static;
    fn dependencies() -> Vec<Key>;
    fn call(&self, resolver: &Resolver) -> BoxFuture<'static, Result<Self::Output, BoxError>>;
}
```

可以當 factory 的 closure：`Fn(A1, ..., An) -> impl Future<Output = Result<T, E>>`，`n` 為 0 到 8，`E: Into<BoxError>`。每個 `Ai` 都是一個 [`Dependency`](#trait-dependency)（`Arc<T>`、`Qualified<T, Q>` 或它們的 `Option`），`Output` 是 `T`。整合 crate 用它實作 `for_root_with` 這類 API（見 [自製整合 crate](../guide/13-without-macros.md#自製整合-crate)）。

| 項目 | 說明 |
|---|---|
| `Output` | factory 產生的型別 |
| `dependencies()` | 每個參數的 `Dependency::key()` |
| `call(resolver)` | 從 `resolver` 取出參數並啟動 factory |

### `bind!`

```rust,ignore
bind!(dyn Trait => Implementation)
```

產生 `ProviderDef::bind::<dyn Trait, Implementation>(...)`：建構 `Implementation`，以 `Arc<dyn Trait>` 提供。在 `#[module(providers = [...])]` 中可以直接使用。

```rust
use hornero::prelude::*;

trait Clock: Send + Sync {}

#[derive(Injectable)]
struct SystemClock;

impl Clock for SystemClock {}

let def = hornero::bind!(dyn Clock => SystemClock);
assert_eq!(def.key(), hornero::Key::of::<dyn Clock>());
```

### `Key`

```rust,ignore
pub struct Key { /* ... */ }
```

以型別識別 provider，可以再加上一個 qualifier 型別。hornero 沒有字串 token：qualifier 是任意的 marker 型別。`T` 可以是 unsized，所以 `Key::of::<dyn Repo>()` 是合法的 key。

| 方法 | 說明 |
|---|---|
| `fn of<T: ?Sized + 'static>() -> Key` | 型別 `T` 的 key |
| `fn qualified<Q: 'static>(self) -> Key` | 同一個型別，加上 qualifier `Q` |
| `fn optional(self) -> Key` | 標記為可選的依賴：沒有任何 provider 時，使用方拿到 `None`，而不是驗證失敗 |
| `fn is_optional(&self) -> bool` | 是否為可選的依賴 |
| `fn type_id(&self) -> TypeId` | 型別的 `TypeId` |
| `fn type_name(&self) -> &'static str` | 完整的 `std::any::type_name` |
| `fn short_name(&self) -> String` | 去掉模組路徑的型別名稱，用於錯誤訊息；有 qualifier 時是 `Pool@Primary` |

以型別與 qualifier 的 `TypeId` 比較相等與雜湊；`optional` 只描述依賴怎麼被要求，不影響比較。`Display` 輸出 `short_name`，`Debug` 輸出完整名稱。

```rust
use hornero::Key;

struct Pool;
struct Primary;
struct Replica;

let primary = Key::of::<Pool>().qualified::<Primary>();
assert_ne!(primary, Key::of::<Pool>());
assert_ne!(primary, Key::of::<Pool>().qualified::<Replica>());
assert_eq!(primary.to_string(), "Pool@Primary");
assert_eq!(Key::of::<Pool>().optional(), Key::of::<Pool>());
```

### `trait Dependency`

```rust,ignore
pub trait Dependency: Sized + 'static {
    fn key() -> Key;
    fn resolve(resolver: &Resolver) -> Result<Self, BoxError>;
}
```

要求一個 provider 的方式：factory 的參數型別，以及 `Resolver::resolve` 接受的型別。

| 型別 | `key()` | 取得 |
|---|---|---|
| `Arc<T>` | `Key::of::<T>()` | `T` 的 provider |
| `Qualified<T, Q>` | `Key::of::<T>().qualified::<Q>()` | 以 qualifier `Q` 註冊的 `T` |
| `Option<D>` | `D::key().optional()` | 沒有任何 provider 時是 `None` |

可選的依賴只在「看不到任何 provider」時是 `None`。有多個來源（歧義）仍然是 bootstrap 錯誤。

### `Qualified`

```rust,ignore
pub struct Qualified<T: ?Sized, Q> { /* Arc<T> */ }
```

以 qualifier `Q` 註冊的 `T`，Nest 的 `@Inject(TOKEN)`。qualifier 是 marker 型別，所以打錯字是編譯錯誤，而不是找不到 provider。實作了 `Deref<Target = T>`、`Clone`，`Debug`（`T: Debug` 時），以及 `From<Qualified<T, Q>> for Arc<T>`。

| 方法 | 說明 |
|---|---|
| `fn new(value: Arc<T>) -> Qualified<T, Q>` | 包裝一個實例 |
| `fn into_inner(self) -> Arc<T>` | 取出共用的實例 |

`Qualified<T, Q>` 出現在三個地方：factory 參數（取得依賴）、`ModuleDef::provide` 與 `#[module(providers = [..])]`（以 qualifier 註冊 class provider），以及 `#[module(exports = [..])]`（export 帶 qualifier 的 provider）。`#[derive(Injectable)]` 的欄位與建構函式參數則維持 `Arc<T>`，改用 `#[inject(qualifier = Q)]`。見 [同一個型別要多個實例](../guide/02-providers.md#同一個型別要多個實例)。

```rust
use hornero::prelude::*;
use hornero::ProviderDef;

struct Url(&'static str);
struct Primary;
struct Replica;
struct Urls(String);

struct AppModule;

impl Module for AppModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provider(ProviderDef::value(Url("primary")).qualified::<Primary>())
            .provider(ProviderDef::value(Url("replica")).qualified::<Replica>())
            .factory(|primary: Qualified<Url, Primary>, replica: Option<Qualified<Url, Replica>>| async move {
                let replica = replica.map_or("none", |url| url.0);
                Ok::<_, BoxError>(Urls(format!("{} {replica}", primary.0)))
            });
    }
}

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
assert_eq!(app.get::<Urls>().unwrap().0, "primary replica");
assert_eq!(app.get_qualified::<Url, Replica>().unwrap().0, "replica");
# }
```

### `Resolver`

```rust,ignore
pub struct Resolver { /* ... */ }
```

把已建構好的依賴交給正在建構的 provider。它只裝著 provider 宣告過的依賴。

| 方法 | 說明 |
|---|---|
| `fn get<T: ?Sized>(&self) -> Result<Arc<T>, BoxError>` | 取得依賴；`T` 沒有宣告在 `dependencies()` 裡時回傳錯誤 |
| `fn resolve<D: Dependency>(&self) -> Result<D, BoxError>` | 取得任何 [`Dependency`](#trait-dependency)：`resolve::<Qualified<T, Q>>()`、`resolve::<Option<Arc<T>>>()`；同樣只能取得宣告過的依賴 |
| `fn owner(&self) -> &str` | 正在建構的 provider 名稱 |

宣告為可選、但沒有任何 provider 的依賴，用 `get` 取得會回傳錯誤，要用 `resolve::<Option<_>>()`。

---

## Request scope

### `RequestContext`

```rust,ignore
pub struct RequestContext { /* ... */ }
```

request head 的快照，以 `Arc<RequestContext>` 注入 request-scoped provider，相當於 Nest 的 `REQUEST`。元件看到的是它被建構當下的 request：快照在每個 guard 與 interceptor 之後作廢並於下次需要時重拍，所以 guard 寫進 extensions 的資料對之後建構的元件（包含 controller 及其依賴）可見，已建構的元件則保留原本的快照。

| 方法 | 說明 |
|---|---|
| `fn method(&self) -> &Method` | HTTP method |
| `fn uri(&self) -> &Uri` | 完整 URI |
| `fn version(&self) -> Version` | HTTP 版本 |
| `fn headers(&self) -> &HeaderMap` | headers |
| `fn extensions(&self) -> &Extensions` | extensions（guard 與 middleware 寫入的資料） |
| `fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<&T>` | `extensions().get::<T>()` 的簡寫 |

### `ScopeReport` 與 `ScopeEntry`

```rust,ignore
pub struct ScopeReport {
    pub entries: Vec<ScopeEntry>,
}

pub struct ScopeEntry {
    pub item: String,
    pub module: String,
    pub declared: bool,
    pub chain: Vec<String>,
}
```

所有 request-scoped 元件與原因。由 `Hornero::validate()` 與 `App::scope_report()` 回傳，啟動時也會寫進 log。

| 項目 | 說明 |
|---|---|
| `ScopeReport::entries` | 每個 request-scoped 元件一筆，依依賴順序排列 |
| `fn is_empty(&self) -> bool` | 沒有任何 request-scoped 元件 |
| `fn find(&self, item: &str) -> Option<&ScopeEntry>` | 依名稱（不含路徑的型別名稱）查詢 |
| `ScopeEntry::item` | 元件名稱 |
| `ScopeEntry::module` | 所屬模組 |
| `ScopeEntry::declared` | `true`：自己宣告為 request；`false`：被傳染 |
| `ScopeEntry::chain` | 被傳染時的依賴鏈，終點是宣告為 request 的 provider |

`Display` 每筆輸出一行，例如 `  TodoService (in TodoModule): request-scoped via TodoService -> AuditLog [request]`。

---

## HTTP

### `trait Controller` 與 `trait ControllerAttrs`

```rust,ignore
pub trait Controller: Injectable {
    fn routes(c: &mut ControllerDef<Self>);
}

pub trait ControllerAttrs: Sized {
    fn configure(c: &mut ControllerDef<Self>);
}
```

`Controller` 是一組由可注入 struct 支撐的路由，由 `#[routes]` 產生，或手寫。`ControllerAttrs` 是 `#[controller(...)]` 產生的 controller 層級設定（前綴、enhancer、metadata），`#[routes]` 產生的 `routes` 會先呼叫它。手寫 controller 不需要實作 `ControllerAttrs`。

### `ControllerDef<C>`

```rust,ignore
pub struct ControllerDef<C> { /* ... */ }
```

傳給 `Controller::routes` 的 builder。

| 方法 | 說明 |
|---|---|
| `fn prefix(&mut self, prefix: impl Into<String>) -> &mut Self` | 所有路由共用的路徑前綴 |
| `fn guard<G: Guard + Injectable>(&mut self) -> &mut Self` | controller 層級的 guard |
| `fn interceptor<I: Interceptor + Injectable>(&mut self) -> &mut Self` | controller 層級的 interceptor |
| `fn filter<F: ExceptionFilter + Injectable>(&mut self) -> &mut Self` | controller 層級的 exception filter |
| `fn meta<T: Send + Sync + 'static>(&mut self, value: T) -> &mut Self` | controller 層級的 metadata；handler 層級的同型別值優先 |
| `fn route<H, Args>(&mut self, method: Method, path: impl Into<String>, name: &'static str, handler: H) -> &mut RouteDef` | 註冊路由；`H: RouteHandler<C, Args>` |
| `fn get` / `post` / `put` / `patch` / `delete(&mut self, path, name, handler) -> &mut RouteDef` | `route` 的簡寫 |

### `RouteDef`

```rust,ignore
pub struct RouteDef { /* ... */ }
```

一條已註冊的路由，用來加上 handler 層級的 enhancer 與 metadata。

| 方法 | 說明 |
|---|---|
| `fn guard<G: Guard + Injectable>(&mut self) -> &mut Self` | handler 層級的 guard，在 controller 的 guard 之後執行 |
| `fn interceptor<I: Interceptor + Injectable>(&mut self) -> &mut Self` | handler 層級的 interceptor，最內層 |
| `fn filter<F: ExceptionFilter + Injectable>(&mut self) -> &mut Self` | handler 層級的 filter，優先於 controller 與全域的 |
| `fn meta<T: Send + Sync + 'static>(&mut self, value: T) -> &mut Self` | handler 層級的 metadata |

### `trait RouteHandler`

```rust,ignore
pub trait RouteHandler<C, Args>: Clone + Send + Sync + 'static { /* 只有 hornero 內部使用的方法 */ }
```

可以當 handler 的 closure：`Fn(Arc<C>, T1, ..., Tn) -> impl Future<Output: IntoResponse>`，`n` 為 0 到 12，`T1..Tn-1` 實作 `FromRequestParts<()>`，`Tn` 實作 `FromRequest<()>`（可以讀取 body）。controller 實例在每個 request 透過 handle 取得：singleton 是一次 `Arc` clone，request-scoped 則從 request scope 取得。

### `trait Guard` 與 `GuardContext`

```rust,ignore
pub trait Guard: Send + Sync + 'static {
    fn can_activate(&self, ctx: &mut GuardContext<'_>) -> impl Future<Output = Result<(), HttpError>> + Send;
}

pub struct GuardContext<'a> {
    pub route: &'a RouteContext,
    pub parts: &'a mut Parts,
}
```

決定 request 能否到達 handler。在 middleware 之後、interceptor 之前執行；回傳 `Err` 時中止 request。

| 項目 | 說明 |
|---|---|
| `can_activate` | 回傳 `Ok(())` 放行；`Err(HttpError)` 以該錯誤回應（通常是 401 或 403） |
| `GuardContext::route` | 路由資訊與 metadata |
| `GuardContext::parts` | request head；可以把資料寫進 `parts.extensions` 給後續元件使用 |

### `trait Interceptor`

```rust,ignore
pub trait Interceptor: Send + Sync + 'static {
    fn intercept(&self, ctx: &RouteContext, req: Request, next: Next) -> impl Future<Output = Response> + Send;
}
```

包住 handler。呼叫 `next.run(req).await` 執行內層；不呼叫就能直接回應。在 guard 之後執行；全域包住 controller，controller 包住 handler。

### `trait ExceptionFilter`

```rust,ignore
pub trait ExceptionFilter: Send + Sync + 'static {
    fn catches(&self, err: &HttpError) -> bool { true }
    fn catch(&self, err: &HttpError, ctx: &RouteContext) -> impl Future<Output = Response> + Send;
}
```

重新渲染帶有 `HttpError` 的回應（來自 handler、guard、interceptor、`Rejection = HttpError` 的 extractor，或 request-scoped 元件建構失敗）。

| 方法 | 說明 |
|---|---|
| `catches` | 這個 filter 是否處理 `err`，相當於 Nest 的 `@Catch(Type)`。預設 `true`（接住所有錯誤）；用 `err.is::<TodoError>()` 只接某種 domain error |
| `catch` | 產生回應。從 `err.render()` 開始修改可以保留預設內容；回傳 `HttpError::not_found(..).into_response()` 這類回應則把錯誤轉成另一個狀態碼 |

**選擇規則**：依序查看 handler、controller、全域的 filter（先 `Hornero::filter`，再模組宣告的 `AppFilter`），同一層依註冊順序，第一個 `catches` 回傳 `true` 的 filter 處理錯誤，其他 filter 不會執行。沒有任何 filter 接住時，回應維持原樣。filter 回應的狀態碼仍是 5xx 時，原本的錯誤會被寫進 log。

```rust
use hornero::prelude::*;

#[derive(Debug)]
struct OutOfStock(u32);

impl std::fmt::Display for OutOfStock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "item {} is out of stock", self.0)
    }
}

impl std::error::Error for OutOfStock {}

#[derive(Injectable)]
struct OutOfStockFilter;

impl ExceptionFilter for OutOfStockFilter {
    fn catches(&self, err: &HttpError) -> bool {
        err.is::<OutOfStock>()
    }

    async fn catch(&self, err: &HttpError, _ctx: &RouteContext) -> Response {
        let item = err.downcast_ref::<OutOfStock>().map_or(0, |e| e.0);
        HttpError::conflict(format!("item {item} is out of stock")).into_response()
    }
}

#[controller("/cart")]
struct CartController;

#[routes]
impl CartController {
    #[post("/{item}")]
    fn add(&self, Path(item): Path<u32>) -> Result<StatusCode, HttpError> {
        let reserved: Result<(), OutOfStock> = Err(OutOfStock(item));
        reserved?; // OutOfStock → 500，再由 filter 轉成 409
        Ok(StatusCode::NO_CONTENT)
    }
}

#[module(controllers = [CartController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let hornero = Hornero::new(AppModule).filter::<OutOfStockFilter>();
let app = hornero_testing::TestApp::from_hornero(hornero).build().await.unwrap();
let res = app.client().post("/cart/3").await;
assert_eq!(res.status(), StatusCode::CONFLICT);
assert_eq!(res.json::<serde_json::Value>()["detail"], "item 3 is out of stock");
# }
```

### `AppGuard`、`AppInterceptor`、`AppFilter`

```rust,ignore
pub struct AppGuard<G>(/* ... */);
pub struct AppInterceptor<I>(/* ... */);
pub struct AppFilter<F>(/* ... */);
```

在模組裡註冊套用到整個應用所有路由的 guard、interceptor、exception filter，相當於 Nest 的 `{ provide: APP_GUARD, useClass: G }`、`APP_INTERCEPTOR`、`APP_FILTER`。它們是 token 型別，不會被建構，放進 `providers` 或傳給 `ModuleDef::provide` 即可。

| 規則 | 說明 |
|---|---|
| 解析 | `G` 在宣告它的模組裡解析，可以依賴該模組沒有 export 的 provider；依賴看不到時，啟動時報 `MissingDependency` |
| 順序 | 先執行 `Hornero::guard` 等 builder 上註冊的，再執行模組宣告的；被 import 的模組先於 import 它的模組，同一模組依宣告順序 |
| 重複 | 同一模組宣告兩次 `AppGuard<G>` 是 `DuplicateProvider`；不同模組各自宣告會各執行一次 |
| 替換 | `override_guard::<G>`（`override_interceptor`、`override_filter` 同理）同樣會替換這些全域 enhancer |

Rust 沒有 Nest 的 multi-provider，同一個 key 只能註冊一次。每個 `AppGuard<X>` 都是不同的型別，所以一個模組可以註冊任意多個全域 guard。

```rust
use hornero::prelude::*;

/// 只在 AuthModule 內可見。
#[derive(Injectable)]
struct TokenStore;

impl TokenStore {
    fn accepts(&self, token: &str) -> bool {
        token == "secret"
    }
}

#[derive(Injectable)]
struct AuthGuard {
    tokens: Arc<TokenStore>,
}

impl Guard for AuthGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        let token = ctx.parts.headers.get("authorization").and_then(|v| v.to_str().ok());
        match token.and_then(|v| v.strip_prefix("Bearer ")) {
            Some(token) if self.tokens.accepts(token) => Ok(()),
            _ => Err(HttpError::unauthorized("missing token")),
        }
    }
}

#[module(providers = [TokenStore, AppGuard<AuthGuard>])]
struct AuthModule;

#[controller("/notes")]
struct NoteController;

#[routes]
impl NoteController {
    #[get("/")]
    fn list(&self) -> &'static str {
        "[]"
    }
}

/// 不知道 AuthModule 的存在，路由仍然受保護。
#[module(controllers = [NoteController])]
struct NotesModule;

#[module(imports = [AuthModule, NotesModule])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
assert_eq!(app.client().get("/notes").await.status(), StatusCode::UNAUTHORIZED);
assert_eq!(app.client().with_bearer("secret").get("/notes").await.status(), StatusCode::OK);
# }
```

### `trait Middleware`

```rust,ignore
pub trait Middleware: Send + Sync + 'static {
    fn handle(&self, req: Request, next: Next) -> impl Future<Output = Response> + Send;
}
```

需要依賴、但與路由無關的 middleware。在 guard 之前執行，不知道是哪條路由。必須是 singleton。不需要依賴的 middleware 請直接用 tower layer。

### `RouteContext`

```rust,ignore
pub struct RouteContext { /* ... */ }
```

正在處理的路由的靜態資訊，guard、interceptor 與 filter 都看得到。

| 方法 | 說明 |
|---|---|
| `fn controller(&self) -> &str` | controller 名稱（不含路徑的型別名稱） |
| `fn handler(&self) -> &'static str` | handler 名稱 |
| `fn method(&self) -> &Method` | HTTP method |
| `fn path(&self) -> &str` | 完整的路徑模式，例如 `/api/todos/{id}` |
| `fn meta<T: Send + Sync + 'static>(&self) -> Option<&T>` | 型別為 `T` 的 metadata；handler 層級優先於 controller 層級 |

### `Metadata`

```rust,ignore
pub struct Metadata { /* ... */ }
```

以型別為 key 的 route metadata 容器，取代 Nest 的 `SetMetadata` 與 `Reflector`。實作了 `Clone` 與 `Default`。

| 方法 | 說明 |
|---|---|
| `fn insert<T: Send + Sync + 'static>(&mut self, value: T)` | 設定型別為 `T` 的值（同型別會被取代） |
| `fn get<T: Send + Sync + 'static>(&self) -> Option<&T>` | 取得型別為 `T` 的值 |

```rust
use hornero::Metadata;

struct Roles(&'static [&'static str]);

let mut meta = Metadata::default();
meta.insert(Roles(&["admin"]));
assert_eq!(meta.get::<Roles>().unwrap().0, ["admin"]);
assert!(meta.get::<String>().is_none());
```

### `Routes`、`RouteInfo` 與 `EnhancerInfo`

```rust,ignore
pub struct Routes { /* ... */ }
pub struct RouteInfo { /* ... */ }
pub struct EnhancerInfo { /* ... */ }
```

應用的所有 controller 路由，模組依 import 順序、路由依註冊順序排列；相當於 Nest 的 Swagger 模組從 metadata 收集的路由清單。清單只由模組圖算出，不建構任何東西：`Hornero::routes()` 在啟動前取得，`App::routes()` 在啟動後取得，provider 與 `mount` factory 也能以內建的 `Arc<Routes>` 注入。[`hornero-openapi`](hornero-openapi.md) 用它產生文件。`mount_router` 與 `mount` 掛上的 axum router 不在清單裡，hornero 不知道它們的路由。三者都實作了 `Clone`。

| `Routes` 的方法 | 說明 |
|---|---|
| `fn prefix(&self) -> &str` | `Hornero::global_prefix` 設定的前綴（已正規化，例如 `/api`），沒有設定時是 `""`；每個 `RouteInfo::path` 都已經包含它 |
| `fn iter(&self) -> std::slice::Iter<'_, RouteInfo>` | 所有路由；`&Routes` 也實作了 `IntoIterator` |
| `fn len(&self) -> usize` | 路由數量 |
| `fn is_empty(&self) -> bool` | 是否沒有任何 controller 路由 |

| `RouteInfo` 的方法 | 說明 |
|---|---|
| `fn method(&self) -> &Method` | HTTP method |
| `fn path(&self) -> &str` | 完整路徑模式，包含 global prefix 與 controller prefix，例如 `/api/todos/{id}` |
| `fn controller(&self) -> &str` | controller 名稱（不含路徑的型別名稱） |
| `fn handler(&self) -> &'static str` | handler 名稱 |
| `fn module(&self) -> &str` | 註冊這個 controller 的模組名稱 |
| `fn meta<T: Send + Sync + 'static>(&self) -> Option<&T>` | 型別為 `T` 的 metadata，與 guard 在 `RouteContext::meta` 看到的相同：handler 層級優先於 controller 層級 |
| `fn guards(&self) -> &[EnhancerInfo]` | 這條路由會執行的 guard，依執行順序：全域的（先 `Hornero::guard`，再模組宣告的 `AppGuard`）、controller 的、handler 的 |

| `EnhancerInfo` 的方法 | 說明 |
|---|---|
| `fn name(&self) -> &str` | enhancer 名稱（不含路徑的型別名稱） |
| `fn type_id(&self) -> TypeId` | enhancer 型別的 `TypeId` |
| `fn is<T: 'static>(&self) -> bool` | enhancer 是否為 `T` |
| `fn meta<T: Send + Sync + 'static>(&self) -> Option<&T>` | enhancer 用 [`Injectable::describe`](#trait-injectable) 描述自己的 metadata，例如 `hornero-openapi` 從實作 `ApiSecurity` 的 guard 取得的 security scheme |

```rust
use hornero::prelude::*;

#[derive(Injectable)]
struct AuthGuard;

impl Guard for AuthGuard {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

#[controller("/todos")]
#[guards(AuthGuard)]
struct TodoController;

#[routes]
impl TodoController {
    #[get("/{id}")]
    async fn find_one(&self, Path(id): Path<u32>) -> String {
        id.to_string()
    }
}

#[module(controllers = [TodoController])]
struct AppModule;

let routes = Hornero::new(AppModule).global_prefix("/api").routes().unwrap();
let route = routes.iter().next().unwrap();
assert_eq!(route.path(), "/api/todos/{id}");
assert_eq!((route.controller(), route.handler()), ("TodoController", "find_one"));
assert!(route.guards()[0].is::<AuthGuard>());
```

---

## 錯誤

### `HttpError`

```rust,ignore
pub struct HttpError { /* ... */ }
```

會變成 HTTP 回應的錯誤，相當於 Nest 的 `HttpException`。預設渲染成 RFC 9457 `application/problem+json`，並附在回應的 extensions 上供 exception filter 使用。exception filter 處理之後，最終回應仍是 5xx 的錯誤會由 hornero 的 router 寫進 log（含 source chain），每個錯誤一次。

| 建構方式 | 說明 |
|---|---|
| `fn new(status: StatusCode, message: impl Into<Cow<'static, str>>) -> Self` | 任意狀態碼 |
| `fn bad_request(message) -> Self` | 400 |
| `fn unauthorized(message) -> Self` | 401；預設帶有 `WWW-Authenticate: Bearer`（可用 `with_header` 取代） |
| `fn forbidden(message) -> Self` | 403 |
| `fn not_found(message) -> Self` | 404 |
| `fn conflict(message) -> Self` | 409 |
| `fn unprocessable(message) -> Self` | 422 |
| `fn internal(message) -> Self` | 500 |
| `fn from_boxed(source: BoxError) -> Self` | 500 `internal server error`，`source` 成為內部原因 |
| `impl<E: std::error::Error + Send + Sync + 'static> From<E>` | 同 `from_boxed`，讓 `?` 可以用 |

| Builder | 說明 |
|---|---|
| `fn with_details(self, details: impl Serialize) -> Self` | 結構化細節，渲染在 `errors` 欄位；序列化失敗時 `details` 維持 `None`，並以 `tracing::warn!` 記錄 |
| `fn with_source(self, source: impl Into<BoxError>) -> Self` | 內部原因：寫進 log，不會送給 client |
| `fn with_header(self, name: HeaderName, value: HeaderValue) -> Self` | 設定回應 header（同名會取代）；`render()` 會套用，filter 可從 `headers()` 取得 |

| 存取與渲染 | 說明 |
|---|---|
| `fn status(&self) -> StatusCode` | 狀態碼 |
| `fn message(&self) -> &str` | 給 client 的訊息（problem 的 `detail`） |
| `fn details(&self) -> Option<&Value>` | 結構化細節 |
| `fn headers(&self) -> &HeaderMap` | 隨錯誤攜帶的回應 header（401 預設含 `WWW-Authenticate: Bearer`） |
| `fn source(&self) -> Option<&(dyn Error + Send + Sync + 'static)>` | 內部原因 |
| `fn downcast_ref<E: Error + 'static>(&self) -> Option<&E>` | 內部原因是 `E` 時取得它：`?` 轉換的錯誤，或傳給 `from_boxed` / `with_source` 的錯誤。不會往 source chain 裡找 |
| `fn is<E: Error + 'static>(&self) -> bool` | 內部原因是否為 `E`，常用在 `ExceptionFilter::catches` |
| `fn to_problem(&self) -> Value` | problem document：`type`、`title`、`status`、`detail`，以及選用的 `errors` |
| `fn render(&self) -> Response` | 預設的 problem+json 回應，不消耗 `self`、不寫 log、不附加 extension |

實作了 `IntoResponse`、`Debug` 與 `Display`（`"404 Not Found: todo 7 not found"`）。

```rust
use hornero::prelude::*;

let err = HttpError::not_found("todo 7 not found").with_source("row missing");
assert_eq!(err.status(), StatusCode::NOT_FOUND);
assert_eq!(err.message(), "todo 7 not found");
assert_eq!(err.to_string(), "404 Not Found: todo 7 not found");
assert_eq!(err.source().unwrap().to_string(), "row missing");
assert_eq!(err.to_problem()["title"], "Not Found");

let err = HttpError::from(std::io::Error::other("disk full")); // `?` 做的轉換
assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
assert!(err.is::<std::io::Error>());
assert_eq!(err.downcast_ref::<std::io::Error>().unwrap().to_string(), "disk full");

let response = err.render();
assert_eq!(response.headers()["content-type"], "application/problem+json");
```

### `BootError`

```rust,ignore
pub struct BootError { /* ... */ }
```

啟動期間所有問題的集合。`Display` 輸出完整的報告（每個問題一段，附上提示）；`Debug` 與 `Display` 相同，所以 `unwrap()` 失敗時會印出可讀的報告。實作了 `std::error::Error`。

| 方法 | 說明 |
|---|---|
| `fn problems(&self) -> &[Problem]` | 所有問題 |

### `ShutdownError`

```rust,ignore
pub struct ShutdownError { /* ... */ }
```

關閉時 `on_module_destroy` 或 `on_application_shutdown` 失敗的集合，由 `App::close()` 回傳（每個 hook 仍然都會執行）。`Display` 以 `failed to shut down application` 開頭，每個問題的呈現方式與 `BootError` 相同；`Debug` 與 `Display` 相同。實作了 `std::error::Error`。

| 方法 | 說明 |
|---|---|
| `fn problems(&self) -> &[Problem]` | 所有 hook 失敗 |

### `enum Problem`

`#[non_exhaustive]`，`match` 時請保留 `_` 分支。

| 變體 | 欄位 | 意義 |
|---|---|---|
| `MissingDependency` | `consumer`、`dependency`、`hint` | 依賴在該模組不可見 |
| `AmbiguousDependency` | `consumer`、`dependency`、`candidates` | 依賴在同一層有多個來源；guard、interceptor、filter 的目標，以及模組 re-export 的型別也適用 |
| `DuplicateProvider` | `provider`、`module` | 同一模組重複註冊同一型別 |
| `CircularDependency` | `path` | provider 之間的循環依賴 |
| `CircularImport` | `path` | 模組之間的循環 import |
| `InvalidExport` | `module`、`export` | export 了既沒提供、也沒 import 的型別 |
| `ScopeViolation` | `item`、`reason`、`chain` | middleware 或 mount 變成 request-scoped、有 hook 的 provider 不是 singleton，或 pinned 的 provider 被傳染 |
| `UnusedOverride` | `provider` | override 了沒有任何模組提供的型別 |
| `InvalidProvider` | `provider`、`reason` | provider 的註冊本身有誤，例如 `with_hooks::<T>()` 的 `T` 不是這個 provider 的型別 |
| `InvalidRoute` | `handler`、`reason` | 舊路徑語法、不支援的 method、axum 拒絕的路由 |
| `DuplicateRoute` | `method`、`path`、`first`、`second` | 兩個 handler 處理同一個 method 與路徑 |
| `Construction` | `item`、`source` | provider 建構失敗 |
| `Hook` | `item`、`hook`、`source` | 生命週期 hook 失敗 |

見 [錯誤處理](../guide/08-errors.md#bootstrap-錯誤)。

---

## 生命週期

### `trait Lifecycle`

```rust,ignore
pub trait Lifecycle: Send + Sync + 'static {
    fn on_module_init(&self) -> impl Future<Output = Result<(), BoxError>> + Send { async { Ok(()) } }
    fn on_application_bootstrap(&self) -> impl Future<Output = Result<(), BoxError>> + Send { async { Ok(()) } }
    fn on_module_destroy(&self) -> impl Future<Output = Result<(), BoxError>> + Send { async { Ok(()) } }
    fn on_application_shutdown(&self) -> impl Future<Output = Result<(), BoxError>> + Send { async { Ok(()) } }
}
```

| Hook | 時機與順序 |
|---|---|
| `on_module_init` | 所有 singleton 建構完成後，依依賴順序 |
| `on_application_bootstrap` | 所有 `on_module_init` 完成後、開始接受 request 前，依依賴順序 |
| `on_module_destroy` | 關閉時、server 停止接受 request 後，依反向依賴順序 |
| `on_application_shutdown` | 所有 `on_module_destroy` 完成後，依反向依賴順序 |

必須以 `#[inject(hooks)]`、`#[injectable(hooks)]`、`const LIFECYCLE = hornero::hooks::<Self>()` 或 `ProviderDef::with_hooks` 啟用。只支援 singleton。見 [生命週期](../guide/07-lifecycle.md)。

### `enum Hook`

```rust,ignore
pub enum Hook { ModuleInit, ApplicationBootstrap, ModuleDestroy, ApplicationShutdown }
```

用於診斷訊息（`Problem::Hook`）。`Display` 輸出對應的方法名稱，例如 `on_module_init`。

### `Shutdown`

```rust,ignore
pub struct Shutdown { /* ... */ }
```

內建的 singleton，以 `Arc<Shutdown>` 注入。實作了 `Clone`、`Debug`、`Default`。

| 方法 | 說明 |
|---|---|
| `fn new() -> Shutdown` | 建立新的 `Shutdown`（應用會自己建立，通常不需要呼叫） |
| `fn trigger(&self)` | 開始關閉：server 停止接受 request，接著執行關閉 hook |
| `fn is_triggered(&self) -> bool` | 是否已經開始關閉 |
| `fn cancelled(&self) -> WaitForCancellationFuture<'_>` | 開始關閉時完成的 future |
| `fn token(&self) -> CancellationToken` | 會跟著應用一起取消的 child token（`tokio_util`） |

```rust
use hornero::Shutdown;

let shutdown = Shutdown::new();
let token = shutdown.token();
assert!(!shutdown.is_triggered());

shutdown.trigger();
assert!(shutdown.is_triggered());
assert!(token.is_cancelled());
```

---

## 工具型別

```rust,ignore
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
```

| 型別 | 用途 |
|---|---|
| `BoxError` | factory、建構函式與生命週期 hook 的錯誤型別；任何 `std::error::Error`、`String`、`&str` 都能用 `?` 或 `.into()` 轉成它 |
| `BoxFuture` | 需要把 async 工作放在 `dyn` 後面時使用，例如 `Arc<dyn Trait>` 的 async 方法 |

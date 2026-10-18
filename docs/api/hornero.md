# `hornero` API 參考

`hornero` 是應用程式唯一需要依賴的 crate。它本身幾乎沒有程式碼，只負責把其他 crate 整理成一個入口：

```rust,ignore
pub use hornero_core::*;                                                     // 所有型別與 trait
pub use hornero_macros::{                                                    // macro
    Injectable, api_body, api_exclude, api_response, api_tags, controller, filters, guards,
    injectable, interceptors, meta, module, routes,
};
#[cfg(feature = "openapi")]
pub use hornero_openapi as openapi;                                          // hornero::openapi::...
pub use axum;                                                             // hornero::axum::...
pub use tower;                                                            // hornero::tower::...
pub mod prelude { /* 見下方 */ }
```

```toml
[dependencies]
hornero = { path = "crates/hornero" }   # 尚未發佈到 crates.io
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

所有 `hornero-core` 的型別都能以 `hornero::` 開頭使用，說明見 [`hornero-core` API 參考](hornero-core.md)。`hornero::axum` 與 `hornero::tower` 是 hornero 使用的同一個版本，不需要自己對齊版本。

- [Feature](#feature)
- [`prelude`](#prelude)
- [`#[derive(Injectable)]`](#deriveinjectable)
- [`#[injectable]`](#injectable)
- [`#[module]`](#module)
- [`#[controller]`](#controller)
- [`#[routes]`](#routes)
- [Enhancer attribute：`#[guards]`、`#[interceptors]`、`#[filters]`、`#[meta]`](#enhancer-attributeguardsinterceptorsfiltersmeta)
- [OpenAPI attribute：`#[api_response]`、`#[api_body]`、`#[api_tags]`、`#[api_exclude]`](#openapi-attributeapi_responseapi_bodyapi_tagsapi_exclude)
- [`bind!`](#bind)
- [macro 展開對照](#macro-展開對照)

---

## Feature

| Feature | 內容 |
|---|---|
| `openapi` | `hornero::openapi`（[`hornero-openapi`](hornero-openapi.md)）。`#[routes]` 從每個 handler 的簽名與 doc comment 推導 OpenAPI 文件，`#[derive(Injectable)]` 與 `#[injectable]` 記錄實作 `ApiSecurity` 的 guard 的 security scheme |
| `swagger-ui` | `openapi`，加上 Swagger UI |

兩者預設都關閉。沒有開啟時，`#[api_response]` 等 attribute 仍然可以寫，只是不產生任何程式碼，所以同一份程式碼在兩種設定下都能編譯。

---

## `prelude`

```rust,ignore
use hornero::prelude::*;
```

| 來源 | 項目 |
|---|---|
| `std` | `Arc` |
| `axum` | `Json`、`Extension`、`Path`、`Query`、`StatusCode`、`IntoResponse`、`Response` |
| hornero 型別 | `App`、`Hornero`、`Module`、`ModuleDef`、`Scope`、`Qualified`、`RequestContext`、`RouteContext`、`GuardContext`、`HttpError`、`BoxError`、`Shutdown`、`AppGuard`、`AppInterceptor`、`AppFilter` |
| hornero trait | `Injectable`、`Controller`、`Guard`、`Interceptor`、`ExceptionFilter`、`Middleware`、`Lifecycle` |
| macro | `Injectable`（derive）、`injectable`、`module`、`controller`、`routes`、`guards`、`interceptors`、`filters`、`meta`、`api_response`、`api_body`、`api_tags`、`api_exclude`、`bind` |

`prelude` 刻意不包含較少用到的項目，例如 `ProviderDef`、`Key`、`Dependency`、`Resolver`、`Metadata`、`BootError`、`Problem`；需要時以 `hornero::ProviderDef` 等路徑使用。

---

## `#[derive(Injectable)]`

為 struct 實作 [`Injectable`](hornero-core.md#trait-injectable)：每個 `Arc<T>` 欄位都是一個依賴，從容器取得；`Option<Arc<T>>` 欄位是可選的依賴。

```rust,ignore
#[derive(Injectable)]
#[inject(scope = request | transient | singleton, hooks)]   // 選用
struct Name {
    dep: Arc<T>,                       // 依賴（T 可以是 dyn Trait）
    maybe: Option<Arc<U>>,             // 可選的依賴：沒有 provider 時是 None
    #[inject(qualifier = Primary)]
    primary: Arc<Pool>,                // 以 qualifier Primary 註冊的 Pool
    #[inject(default)]
    state: Mutex<Vec<String>>,         // Default::default()
    #[inject(with = Instant::now())]
    started: Instant,                  // 任意運算式
}
```

### struct 層級：`#[inject(...)]`

| 選項 | 效果 |
|---|---|
| `scope = singleton` | 預設的 scope，但**明確寫出**時會固定（pin）：被傳染成 request scope 會是 bootstrap 錯誤 |
| `scope = request` | 每個 request 一個實例 |
| `scope = transient` | 每個依賴它的元件一個實例 |
| `hooks` | 啟用 [`Lifecycle`](hornero-core.md#trait-lifecycle) hook，需要另外寫 `impl Lifecycle for Name` |

### 欄位層級：`#[inject(...)]`

| 寫法 | 欄位值 |
|---|---|
| （沒有屬性，型別是 `Arc<T>`） | 從容器取得的 `T` |
| （沒有屬性，型別是 `Option<Arc<T>>`） | 從容器取得的 `T`；看不到任何 provider 時是 `None`（Nest 的 `@Optional()`） |
| `#[inject(qualifier = Q)]` | 以 qualifier `Q` 註冊的 `T`（Nest 的 `@Inject(TOKEN)`）；可以搭配 `Arc<T>` 或 `Option<Arc<T>>` |
| `#[inject(default)]` | `Default::default()` |
| `#[inject(with = expr)]` | `expr`，每次建構時求值 |

不是 `Arc<T>` 或 `Option<Arc<T>>` 的欄位一定要加上 `default` 或 `with`，否則是編譯錯誤。判斷依據是型別的寫法，`std::sync::Arc<T>`、`Arc<T>` 都可以，型別別名則不行。`qualifier` 不能和 `default`、`with` 一起用。

支援具名欄位、tuple struct 與 unit struct，以及泛型 struct。不支援 enum 與 union，請手寫 `impl Injectable`。

```rust
use std::sync::Mutex;
use std::time::Instant;

use hornero::prelude::*;

#[derive(Injectable)]
struct Clock;

#[derive(Injectable)]
#[inject(scope = transient)]
struct Stopwatch {
    clock: Arc<Clock>,
    #[inject(default)]
    laps: Mutex<Vec<u64>>,
    #[inject(with = Instant::now())]
    started: Instant,
}

#[derive(Injectable)]
struct Pair(Arc<Clock>, Arc<Stopwatch>);

#[module(providers = [Clock, Stopwatch, Pair])]
struct AppModule;

let report = Hornero::new(AppModule).validate().unwrap();
assert!(report.is_empty());
```

---

## `#[injectable]`

用在兩個地方。

### 在 `impl` 區塊上：建構函式注入

```rust,ignore
#[injectable(scope = request, hooks)]   // 選項與 #[inject(...)] 相同，都可省略
impl Name {
    async fn new(a: Arc<A>, #[inject(qualifier = Primary)] b: Arc<B>, c: Option<Arc<C>>) -> Result<Self, E> { /* ... */ }
}
```

| 規則 | 說明 |
|---|---|
| 建構函式 | 名為 `new` 的函式；或用 `#[constructor]` 標記另一個函式（最多一個） |
| 參數 | 每個參數都必須是 `Arc<T>`（依賴）或 `Option<Arc<T>>`（可選的依賴），可以加上 `#[inject(qualifier = Q)]`；不能有 `self`，不能是泛型 |
| `async` | 可以是 `async fn`，也可以是一般 `fn` |
| 回傳型別 | `Self`；或 `Result<Self, E>`（包括 `anyhow::Result<Self>` 這類別名），`E: Into<BoxError>` |
| 位置 | 必須是 inherent impl，不能是 trait impl |

`impl` 區塊裡的其他方法維持原樣。`#[constructor]` 屬性會被移除，所以標記的函式仍然可以在別處直接呼叫。

```rust
use hornero::prelude::*;

#[derive(Injectable)]
struct Settings;

struct Mailer {
    from: String,
}

#[injectable]
impl Mailer {
    #[constructor]
    async fn connect(_settings: Arc<Settings>) -> Result<Self, std::io::Error> {
        Ok(Mailer { from: "noreply@example.com".into() })
    }

    fn from(&self) -> &str {
        &self.from
    }
}

#[module(providers = [Settings, Mailer])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
assert_eq!(app.get::<Mailer>().unwrap().from(), "noreply@example.com");
# }
```

### 在 struct 上

`#[injectable(scope = request)] struct Name { ... }` 等於 `#[derive(Injectable)] #[inject(scope = request)] struct Name { ... }`，欄位規則相同。

---

## `#[module]`

宣告一個模組：產生 [`impl Module`](hornero-core.md#trait-module)。

```rust,ignore
#[module(
    imports = [AuthModule, ConfigModule::for_root(source).load::<AppConfig>()],
    providers = [
        TodoService,                                 // class provider
        AppGuard<AuthGuard>,                         // 全域 guard（Nest 的 APP_GUARD）
        bind!(dyn TodoRepo => SeaOrmTodoRepo),       // 任何產生 ProviderDef 的 macro
        value(Clock::system()),                      // 固定值
        factory(|cfg: Arc<AppConfig>| async move { Ok::<_, BoxError>(Client::new(&cfg.url)) }),
        provider(ProviderDef::value(42_u32).pinned()), // 任何 ProviderDef 運算式
        value(Pool::new(PRIMARY)).qualified::<Primary>(), // 後面可以接 ProviderDef 的 builder 方法
        Qualified<Archiver, Primary>,                // 以 qualifier 註冊的 class provider
    ],
    controllers = [TodoController],
    exports = [TodoService, dyn TodoRepo, Qualified<Pool, Primary>, module(AuthModule)],
    middlewares = [RequestIdMiddleware],
    global,
)]
pub struct TodoModule;
```

所有選項都可省略，順序不限；沒有任何選項時寫 `#[module]`。

| 選項 | 內容 | 對應的 `ModuleDef` 呼叫 |
|---|---|---|
| `imports = [..]` | 模組**值**（運算式），例如 `AuthModule`、`CacheModule::new(1024)` | `m.import(expr)` |
| `providers = [..]` | 見下表 | |
| `controllers = [..]` | controller 型別 | `m.controller::<T>()` |
| `exports = [..]` | provider 型別、`Qualified<T, Q>`（帶 qualifier 的 provider），或 `module(Type)` 轉出 import 的模組 | `m.export::<T>()` / `m.export_qualified::<T, Q>()` / `m.export_module::<T>()` |
| `middlewares = [..]` | DI middleware 型別，先列的在外層 | `m.middleware::<T>()` |
| `global` | 讓 exports 對所有模組可見 | `m.global()` |

`providers` 的元素：

| 寫法 | 意義 | 對應的呼叫 |
|---|---|---|
| `Type` | class provider，`Type: Injectable` | `m.provide::<Type>()` |
| `Qualified<T, Q>` | 以 qualifier `Q` 註冊的 class provider，`T: Injectable` | `m.provide::<Qualified<T, Q>>()` |
| `AppGuard<G>`、`AppInterceptor<I>`、`AppFilter<F>` | 套用到整個應用所有路由的 enhancer，在本模組解析（Nest 的 `APP_GUARD`、`APP_INTERCEPTOR`、`APP_FILTER`）；見 [`AppGuard`](hornero-core.md#appguardappinterceptorappfilter) | `m.provide::<AppGuard<G>>()` |
| `bind!(dyn Trait => Impl)`，或任何 `name!(..)` | 產生 `ProviderDef` 的 macro | `m.provider(name!(..))` |
| `value(expr)` | 固定值（`useValue`） | `m.provider(ProviderDef::value(expr))` |
| `factory(closure)` | async factory，參數為注入的 [`Dependency`](hornero-core.md#trait-dependency)（`useFactory`） | `m.provider(ProviderDef::factory(closure))` |
| `provider(expr)` | 任何 `ProviderDef` 運算式，例如需要 `.scope(..)` 的 factory | `m.provider(expr)` |

`bind!(..)`、`value(..)`、`factory(..)` 與 `provider(..)` 後面可以接 [`ProviderDef`](hornero-core.md#providerdef) 的 builder 方法，例如 `value(Pool::new(url)).qualified::<Primary>()`、`factory(make_client).scope(Scope::Transient)`。

需要條件式註冊、迴圈或 `layer` 等 macro 沒有提供的功能時，改為手寫 `impl Module`，見 [不用 macro](../guide/13-without-macros.md)。

```rust
use hornero::ProviderDef;
use hornero::prelude::*;

trait Clock: Send + Sync {
    fn now(&self) -> u64;
}

#[derive(Injectable)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> u64 {
        1_700_000_000
    }
}

struct Greeting(&'static str);
struct Banner(String);
struct RequestId(u64);

#[module(
    providers = [
        bind!(dyn Clock => FixedClock),
        value(Greeting("hello")),
        factory(|greeting: Arc<Greeting>, clock: Arc<dyn Clock>| async move {
            Ok::<_, BoxError>(Banner(format!("{} @ {}", greeting.0, clock.now())))
        }),
        provider(ProviderDef::factory(|| async { Ok::<_, BoxError>(RequestId(7)) }).scope(Scope::Transient)),
    ],
    exports = [Banner, dyn Clock],
)]
struct CoreModule;

#[module(imports = [CoreModule])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
assert_eq!(app.get::<Banner>().unwrap().0, "hello @ 1700000000");
# }
```

---

## `#[controller]`

宣告 controller struct：derive `Injectable`，並產生 [`ControllerAttrs`](hornero-core.md#trait-controller-與-trait-controllerattrs)（前綴與 controller 層級的 enhancer）。路由由同一個型別的 `#[routes]` 區塊定義。

```rust,ignore
#[controller("/todos")]                // 路徑前綴，選用：#[controller]
#[guards(AuthGuard, RolesGuard)]       // 以下都是選用，寫在 #[controller] 下方
#[interceptors(Logging)]
#[filters(ProblemFilter)]
#[meta(Roles(&["admin"]))]
#[meta(Public)]
struct TodoController {
    todos: Arc<TodoService>,           // 欄位規則與 #[derive(Injectable)] 相同
}
```

`#[controller]` 的參數是路徑前綴；省略時為根路徑。下方疊加的 enhancer attribute 套用到這個 controller 的每一條路由，說明見 [Enhancer attribute](#enhancer-attributeguardsinterceptorsfiltersmeta)。

enhancer 也可以寫成參數，效果相同；兩種寫法並用時，參數裡的先執行：

| 參數 | 等同於 |
|---|---|
| `guards = [A, B]` | `#[guards(A, B)]` |
| `interceptors = [A, B]` | `#[interceptors(A, B)]` |
| `filters = [A, B]` | `#[filters(A, B)]` |
| `meta = [X, Y]` | `#[meta(X)]` 加 `#[meta(Y)]` |

設定 controller 的 scope：在 `#[controller]` **下方**加上 `#[injectable(scope = request)]`，或使用 `#[inject(scope = request)]`。欄位上的 `#[inject(default)]`、`#[inject(with = ..)]` 也可以使用。

---

## `#[routes]`

用在 controller 的 inherent `impl` 區塊上，把標了路由屬性的方法變成路由，並產生 [`impl Controller`](hornero-core.md#trait-controller-與-trait-controllerattrs)。struct 必須有 `#[controller]`，每個 controller 只能有一個 `#[routes]` 區塊。`#[routes]` 本身不接受參數。

```rust,ignore
#[routes]
impl TodoController {
    #[get("/{id}")]
    async fn find_one(&self, Path(id): Path<u64>) -> Result<Json<Todo>, HttpError> { /* ... */ }

    #[post("/")]
    #[guards(RolesGuard)]
    #[interceptors(Audit)]
    #[filters(ConflictFilter)]
    #[meta(Roles(&["admin"]))]
    async fn create(&self, Json(input): Json<NewTodo>) -> Result<(StatusCode, Json<Todo>), HttpError> { /* ... */ }

    fn helper(&self) {}                // 沒有路由屬性：一般方法，維持原樣
}
```

### 路由屬性

每個方法最多一個。

| 屬性 | HTTP method |
|---|---|
| `#[get("/path")]` | `GET` |
| `#[post("/path")]` | `POST` |
| `#[put("/path")]` | `PUT` |
| `#[patch("/path")]` | `PATCH` |
| `#[delete("/path")]` | `DELETE` |
| `#[head("/path")]` | `HEAD` |
| `#[options("/path")]` | `OPTIONS` |
| `#[route("METHOD", "/path")]` | 任意 method 字串；必須是 RFC 9110 的 token（非空，僅限英數字與 ``!#$%&'*+-.^_`|~``），否則編譯錯誤 |

路徑使用 axum 0.8 的語法（`/{id}`、`/{*rest}`），並接在 controller 前綴與 `Hornero::global_prefix` 之後，接法見 [Controller 與路由](../guide/04-controllers.md#路徑)。舊的 `:id` 語法與 axum 無法比對的自訂 method 都會在啟動時回報為 `invalid route`。

### Enhancer 屬性

路由方法上可以加 `#[guards(..)]`、`#[interceptors(..)]`、`#[filters(..)]`、`#[meta(..)]`，只作用在這條路由，必須和路由屬性放在同一個方法上。說明見下一節。OpenAPI 用的 `#[api_response(..)]` 等也一樣，見 [OpenAPI attribute](#openapi-attributeapi_responseapi_bodyapi_tagsapi_exclude)。

### Handler 的簽名

| 部分 | 規則 |
|---|---|
| receiver | `&self`，或 `self: Arc<Self>`（需要把 controller 移進 `tokio::spawn` 時）；不能是 `&mut self` 或 `self` |
| `async` | 可以是 `async fn`，也可以是一般 `fn` |
| 其他參數 | axum extractor，最多 12 個；只有最後一個可以讀取 body（`Json`、`Form`、`String`…） |
| 回傳型別 | 任何 `IntoResponse`，常見的是 `Result<T, HttpError>` |
| 泛型 | 不支援 |

```rust
use hornero::axum::extract::Request;
use hornero::axum::middleware::Next;
use hornero::prelude::*;

struct Audited(&'static str);

#[derive(Injectable)]
struct AuditHeader;

impl Interceptor for AuditHeader {
    async fn intercept(&self, ctx: &RouteContext, req: Request, next: Next) -> Response {
        let label = ctx.meta::<Audited>().map_or("none", |audited| audited.0);
        let mut response = next.run(req).await;
        response.headers_mut().insert("x-audit", label.parse().unwrap());
        response
    }
}

#[controller("/items")]
#[interceptors(AuditHeader)]
#[meta(Audited("controller"))]
struct ItemController;

#[routes]
impl ItemController {
    #[get("/{id}")]
    fn find(&self, Path(id): Path<u32>) -> String {
        format!("item {id}")
    }

    #[route("POST", "/")]
    #[meta(Audited("create"))]
    async fn create(self: Arc<Self>, body: String) -> (StatusCode, String) {
        (StatusCode::CREATED, body)
    }
}

#[module(controllers = [ItemController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();

let res = app.client().get("/items/3").await;
assert_eq!(res.text(), "item 3");
assert_eq!(res.header("x-audit"), Some("controller"));

let res = app.client().post("/items").body("pen").await;
assert_eq!(res.status(), StatusCode::CREATED);
assert_eq!(res.header("x-audit"), Some("create"));
# }
```

---

## Enhancer attribute：`#[guards]`、`#[interceptors]`、`#[filters]`、`#[meta]`

對應 Nest 的 `@UseGuards`、`@UseInterceptors`、`@UseFilters` 與 `@SetMetadata`（或 `@Roles('admin')` 這類自訂 decorator）。寫在兩個地方，語法相同：

- **controller 層級**：`#[controller]` 下方，套用到 controller 的每一條路由。
- **handler 層級**：`#[routes]` 區塊裡的路由方法上，只套用到這條路由。

| attribute | 內容 | 執行順序 |
|---|---|---|
| `#[guards(A, B)]` | 一個或多個 guard 型別 | 全域 → controller → handler，同一層由上到下、由左到右 |
| `#[interceptors(A, B)]` | 一個或多個 interceptor 型別 | 全域包住 controller，controller 包住 handler；同一層先列的在外層 |
| `#[filters(A, B)]` | 一個或多個 exception filter 型別 | handler → controller → 全域，第一個 `catches` 錯誤的 filter 處理它 |
| `#[meta(expr)]` | 一個 metadata 值（任何運算式） | 以型別為 key；同型別時 handler 層級優先於 controller 層級 |

同一種 attribute 可以寫多次。列出的型別必須實作對應的 trait 與 `Injectable`。

這些 attribute 由 `#[controller]` 與 `#[routes]` 讀取並移除。放錯位置時（寫在 `#[controller]` 上方、寫在沒有 `#[controller]` 的 struct 上，或寫在沒有 `#[routes]` 的 impl 區塊裡），會得到指出正確位置的編譯錯誤：

```text
error: `#[guards(...)]` must come after `#[controller(...)]`: put `#[controller]` first and the enhancers below it
  --> src/todo/controller.rs:12:1
   |
12 | #[guards(AuthGuard)]
   | ^^^^^^^^^^^^^^^^^^^^
```

---

## OpenAPI attribute：`#[api_response]`、`#[api_body]`、`#[api_tags]`、`#[api_exclude]`

補充 `openapi` feature 從簽名推導不出來的部分，對應 Nest 的 `@ApiResponse`、`@ApiBody`、`@ApiTags` 與 `@ApiExcludeEndpoint`。和 enhancer attribute 一樣由 `#[controller]` 與 `#[routes]` 讀取並移除，放錯位置會得到指出正確位置的編譯錯誤。

| attribute | 位置 | 內容 |
|---|---|---|
| `#[api_response(status)]` | handler、`#[controller]` 下方 | 一個回應。選項：`description = ".."`、`body = Type`、`content_type = ".."`（需要 `body`，預設 `application/json`） |
| `#[api_body(Type)]` | handler | request body，取代推導的結果。選項：`content_type = ".."`，預設 `application/json` |
| `#[api_tags("A", "B")]` | handler、`#[controller]` 下方 | 取代預設的 tag（controller 名稱去掉 `Controller`）；handler 層級優先 |
| `#[api_exclude]` | handler、`#[controller]` 下方 | 不列入文件 |

`#[api_response]` 的規則：

- 狀態碼必須在 100 到 599 之間，否則編譯錯誤。
- 沒有 `body` 時，2xx（204 除外）沿用從回傳型別推導的 body，4xx 與 5xx 是 `ProblemDetails`（problem+json），其他沒有 body。
- 宣告了任何 2xx 時，不再自動加上 200。
- 寫在 `#[controller]` 下方時套用到每一條路由；handler 自己有同一個狀態碼時以 handler 為準。
- `description` 預設是狀態碼的標準說明。

```rust
use hornero::openapi::{OpenApiConfig, ToSchema, document};
use hornero::prelude::*;
use serde::Serialize;

#[derive(Serialize, ToSchema)]
struct Item {
    id: u32,
}

#[controller("/items")]
#[api_tags("Catalog")]
#[api_response(401, description = "Sign in first")]
struct ItemController;

#[routes]
impl ItemController {
    #[post("/")]
    #[api_response(201, description = "The new item")]
    #[api_response(409)]
    async fn create(&self) -> (StatusCode, Json<Item>) {
        (StatusCode::CREATED, Json(Item { id: 1 }))
    }
}

#[module(controllers = [ItemController])]
struct AppModule;

let doc = document(&Hornero::new(AppModule).routes().unwrap(), &OpenApiConfig::new("Items", "1.0"));
let responses = &doc.paths.paths["/items"].post.as_ref().unwrap().responses.responses;
let statuses: Vec<&str> = responses.keys().map(String::as_str).collect();
assert_eq!(statuses, ["201", "401", "409"]);
```

推導規則與更多範例見 [OpenAPI](../guide/16-openapi.md)。

---

## `bind!`

```rust,ignore
bind!(dyn Trait => Implementation)
```

建構 `Implementation`，以 `Arc<dyn Trait>` 提供。展開成 `hornero::ProviderDef::bind`，詳見 [`hornero-core`](hornero-core.md#bind)。在 `#[module(providers = [..])]` 中可以直接寫，不需要 `provider(..)`。

---

## macro 展開對照

macro 只是 [L1 API](../guide/13-without-macros.md) 的簡寫。

| macro | 產生 |
|---|---|
| `#[derive(Injectable)]` | `impl Injectable`：`dependencies()` 列出每個 `Arc<T>` 欄位的 `Key`，`construct()` 逐一 `resolver.get::<T>()?`；`#[inject(qualifier = Q)]` 加上 `.qualified::<Q>()`、`Option<Arc<T>>` 加上 `.optional()`，改用 `resolver.resolve::<..>()?` 取值 |
| `#[inject(scope = s)]` | `const SCOPE = Scope::S;` 與 `const SCOPE_PINNED = true;` |
| `#[inject(hooks)]` | `const LIFECYCLE = hornero::hooks::<Self>();` |
| `#[injectable] impl` | 原本的 `impl`，加上呼叫建構函式的 `impl Injectable` |
| `#[module(..)]` | `impl Module`：依序呼叫 `global`、`import`、provider 註冊、`controller`、`middleware`、`export` |
| `#[controller(..)]` | `#[derive(Injectable)]`，加上設定前綴與 enhancer 的 `impl ControllerAttrs`；下方的 `#[guards(G)]` 等變成 `c.guard::<G>()` 等呼叫 |
| `#[routes]` | `impl Controller`：先呼叫 `ControllerAttrs::configure`，再為每個方法呼叫 `c.route(method, path, name, closure)` 與 `.guard::<G>()` 等 |
| `openapi` feature | `#[routes]` 為每條路由多加一個 `.meta(..)`，內容是從簽名、doc comment 與 `#[api_*]` 推導的文件；`#[controller]` 的 `#[api_*]` 變成 `c.meta(..)`；`#[derive(Injectable)]` 多產生 `fn describe`，記錄 `ApiSecurity` scheme |

產生的程式碼一律使用 `::hornero::` 路徑，所以使用 macro 的 crate 必須依賴 `hornero`（而不是只依賴 `hornero-core`）。

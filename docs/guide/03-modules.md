# 3. 模組

模組是封裝的單位：它擁有一組 provider 與 controller，並決定哪些 provider 對外公開。應用就是一棵以根模組為起點的模組樹。

## `#[module]` 的選項

```rust
use hornero::prelude::*;
use hornero_config::{ConfigModule, Source};
# use hornero::axum::extract::Request;
# use hornero::axum::middleware::Next;
# #[module]
# struct AuthModule;
# #[derive(Injectable)]
# struct RequestIdMiddleware;
# impl Middleware for RequestIdMiddleware {
#     async fn handle(&self, req: Request, next: Next) -> Response { next.run(req).await }
# }
# #[derive(Injectable)]
# struct SqlTodoRepo;
# trait TodoRepo: Send + Sync {}
# impl TodoRepo for SqlTodoRepo {}
# #[derive(Injectable)]
# struct TodoService;
# #[controller("/todos")]
# struct TodoController;
# #[routes]
# impl TodoController {}

#[module(
    imports = [AuthModule, ConfigModule::for_root(Source::new().toml("config.toml"))],   // 要使用哪些模組的 exports（任意運算式）
    providers = [TodoService, bind!(dyn TodoRepo => SqlTodoRepo)],
    controllers = [TodoController],
    exports = [TodoService, module(AuthModule)],          // 對外公開的 provider；module(M) 轉出整個模組
    middlewares = [RequestIdMiddleware],                  // 只包住本模組 controller 的 DI middleware
    global,                                               // exports 對所有模組可見
)]
pub struct TodoModule;
# Hornero::new(TodoModule).validate().unwrap();
```

每個選項都可以省略。`#[module]` 展開後只是一個 `impl Module`，對應的手寫 API 見 [不用 macro](13-without-macros.md)。

## Import 與 export

provider 預設**只在自己的模組可見**。其他模組要使用它，必須同時滿足兩件事：提供它的模組把它放進 `exports`，而且使用方 import 了那個模組。

```rust
use hornero::prelude::*;

// ---- users ----
#[derive(Injectable)]
struct UserRepo;

#[derive(Injectable)]
struct UserService {
    repo: Arc<UserRepo>,
}

#[module(providers = [UserRepo, UserService], exports = [UserService])]
struct UsersModule;

// ---- orders ----
#[derive(Injectable)]
struct OrderService {
    users: Arc<UserService>, // UsersModule export 的
}

#[module(imports = [UsersModule], providers = [OrderService])]
struct OrdersModule;

// ---- 根模組 ----
#[module(imports = [UsersModule, OrdersModule])]
struct AppModule;

Hornero::new(AppModule).validate().unwrap();
```

`UsersModule` 被 import 了兩次（根模組一次、`OrdersModule` 一次），但只會有一個 `UsersModule`，裡面的 singleton 也只建構一次。

`UserRepo` 沒有 export，所以 `OrdersModule` 看不到它。直接依賴它會在啟動時失敗，錯誤訊息會告訴你怎麼修：

```rust
use hornero::prelude::*;

#[derive(Injectable)]
struct UserRepo;

#[module(providers = [UserRepo])]
struct UsersModule;

#[derive(Injectable)]
struct OrderService {
    repo: Arc<UserRepo>, // UserRepo 是 UsersModule 私有的
}

#[module(imports = [UsersModule], providers = [OrderService])]
struct OrdersModule;

let err = Hornero::new(OrdersModule).validate().unwrap_err();
assert_eq!(
    err.to_string(),
    "failed to bootstrap application: 1 problem

[1] missing dependency
    OrderService (in OrdersModule) requires `UserRepo`, but no provider of it is visible there.
    hint: `UserRepo` is provided by UsersModule but not exported; add it to the exports of UsersModule and import UsersModule in OrdersModule.
"
);
```

## 可見性規則

在模組 `M` 裡解析型別 `T` 時，依序查找三層，找到就停：

1. **local**：`M` 自己的 provider。
2. **imports**：`M` import 的模組所 export 的 provider（包含它們轉出的東西）。
3. **global**：所有 global 模組 export 的 provider，以及內建的 `Shutdown`、`RequestContext`、`Routes`。

因此：

- **local 會遮蔽 import**：模組自己提供的 `Clock` 優先於 import 進來的 `Clock`。
- **同一層有多個來源是錯誤**：兩個 import 的模組都 export 了 `Database`，hornero 不會隨便挑一個，而是回報 `ambiguous dependency`，並列出所有候選。guard、interceptor、filter 的目標，以及 `exports` 轉出的型別也一樣。
- **export 一個自己沒有的東西是錯誤**：`exports` 只能列出自己提供的 provider，或 import 進來的 provider（也就是轉出）。

```rust
use hornero::prelude::*;

struct Database(&'static str);

#[module(providers = [value(Database("primary"))], exports = [Database])]
struct PrimaryModule;

#[module(providers = [value(Database("legacy"))], exports = [Database])]
struct LegacyModule;

#[derive(Injectable)]
struct Reports {
    db: Arc<Database>,
}

#[module(imports = [PrimaryModule, LegacyModule], providers = [Reports])]
struct ReportsModule;

let err = Hornero::new(ReportsModule).validate().unwrap_err();
assert!(err.to_string().contains(
    "Reports (in ReportsModule) requires `Database`, which is visible from several sources:
      - Database (in PrimaryModule)
      - Database (in LegacyModule)"
));
```

## 轉出（re-export）

一個模組可以把 import 進來的東西再公開出去，讓使用者只需要 import 一個「門面」模組：

```rust
use hornero::prelude::*;

struct Database;
struct Cache;

#[module(providers = [value(Database)], exports = [Database])]
struct DatabaseModule;

#[module(providers = [value(Cache)], exports = [Cache])]
struct CacheModule;

// 轉出整個 DatabaseModule，以及 CacheModule 的 Cache
#[module(imports = [DatabaseModule, CacheModule], exports = [module(DatabaseModule), Cache])]
struct InfraModule;

#[derive(Injectable)]
struct Service {
    db: Arc<Database>,
    cache: Arc<Cache>,
}

#[module(imports = [InfraModule], providers = [Service])]
struct FeatureModule;

Hornero::new(FeatureModule).validate().unwrap();
```

## Global 模組

設定、資料庫連線這類每個模組都會用到的東西，可以放進 global 模組。global 模組 export 的 provider 對所有模組可見，不需要逐一 import：

```rust
use hornero::prelude::*;

struct AppName(&'static str);

#[module(global, providers = [value(AppName("shop"))], exports = [AppName])]
struct CoreModule;

#[derive(Injectable)]
struct Banner {
    name: Arc<AppName>, // 沒有 import CoreModule 也看得到
}

#[module(providers = [Banner])]
struct ShopModule;

// global 模組仍然要在模組樹的某處被 import 一次，通常是根模組
#[module(imports = [CoreModule, ShopModule])]
struct AppModule;

Hornero::new(AppModule).validate().unwrap();
```

global 是刻意的捷徑，請留給真正橫切的關注點。`ConfigModule` 與 `SeaOrmModule` 預設是 global，可以用 `.local()` 關閉。

## 動態模組

NestJS 的 `forRoot(options)` 在 hornero 裡就是**帶著設定的 struct**。帶參數的模組通常手寫 `impl Module`，在 `define` 裡用 `ModuleDef` builder 描述內容，並以 `fn identity()` 宣告每次呼叫都是獨立的模組（見下方「模組的身分」）：

```rust
use hornero::prelude::*;

pub struct CacheConfig {
    pub capacity: usize,
}

#[derive(Injectable)]
pub struct Cache {
    config: Arc<CacheConfig>,
}

impl Cache {
    pub fn capacity(&self) -> usize {
        self.config.capacity
    }
}

pub struct CacheModule {
    capacity: usize,
}

impl CacheModule {
    pub fn new(capacity: usize) -> Self {
        Self { capacity }
    }
}

impl Module for CacheModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide_value(CacheConfig { capacity: self.capacity })
            .provide::<Cache>()
            .export::<Cache>();
    }

    fn identity(&self) -> Identity {
        Identity::Instance
    }
}

#[module(imports = [CacheModule::new(1024)])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
assert_eq!(app.get::<Cache>().unwrap().capacity(), 1024);
# }
```

撰寫動態模組時要注意：

- **`define` 只負責描述**。`validate()` 與 `build()` 都會呼叫它，所以不要在裡面做 I/O。連線、讀檔這類工作請放進 factory（`m.factory(...)`），失敗時會變成 bootstrap 錯誤。
- **模組的身分**決定哪些 import 是同一個模組，不需要自己維護 id。預設的 `Identity::Auto`：零大小的模組（沒有欄位，或只有 `PhantomData`，例如 `struct UsersModule;`）以型別為身分，import 幾次都是同一個實例，同 Nest 的靜態模組；帶資料的模組可以 import 一次，同一型別被 import 第二次就是 bootstrap 錯誤（`module imported more than once`），要求在 `fn identity()` 宣告。合併會丟掉第二份設定、拆開會複製 singleton 與 global enhancer，兩者都不是安全的預設：

| `fn identity()` | 意義 |
|---|---|
| `Identity::Instance` | 每次 import 都是獨立的模組，同 Nest 的 `forRoot()` |
| `Identity::key(k)` | 同型別、key 相等（`==`）的 import 是同一個模組，以第一個為準；key 要涵蓋所有會讓兩次 import 不同的欄位 |
| `Identity::Type` | 同型別的 import 都是同一個模組，以第一個為準 |

宣告 `Identity::Instance` 之後，兩個 `CacheModule` 各自提供自己的 `Cache`：

```rust
use hornero::prelude::*;

struct Cache(usize);

struct CacheModule(usize);

impl Module for CacheModule {
    fn define(&self, m: &mut ModuleDef) {
        m.provide_value(Cache(self.0)).export::<Cache>();
    }

    fn identity(&self) -> Identity {
        Identity::Instance
    }
}

#[derive(Injectable)]
struct Sessions {
    cache: Arc<Cache>,
}

#[derive(Injectable)]
struct Tokens {
    cache: Arc<Cache>,
}

#[module(imports = [CacheModule(1)], providers = [Sessions], exports = [Sessions])]
struct SessionsModule;

#[module(imports = [CacheModule(60)], providers = [Tokens], exports = [Tokens])]
struct TokensModule;

#[module(imports = [SessionsModule, TokensModule])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
assert_eq!(app.get::<Sessions>().unwrap().cache.0, 1);
assert_eq!(app.get::<Tokens>().unwrap().cache.0, 60);
# }
```

- **同一個模組看得到兩個實例**，而兩者 export 同一個型別時，使用方會遇到歧義。要同時存在，就讓 provider 帶不同的型別（例如泛型 tag，`Db<Tag>`）。
- **要共用同一個實例**，就在一個模組裡 import 一次，再以 `exports = [module(CacheModule)]`（`m.export_module::<CacheModule>()`）轉出，同 Nest 文件 re-export 動態模組的做法（`exports` 寫模組 class，不再呼叫 `forRoot()`）。`export_module` 依型別比對，`CacheModule::new(..)` 這類值也轉得出去；模組自己 export 的同型別 provider 優先於轉出的。若要讓內容相同的 import 自動合併，回傳 `Identity::key(..)`（`ConfigModule::for_feature` 就是這樣讓相同的 feature 去重）。
- **宣告 global enhancer 的模組**（`AppGuard<G>` 等）不能有多個實例：每個實例都會在每個 route 上再執行一次，所以是 bootstrap 錯誤（`duplicate global enhancer`）。
- **`Identity::Instance` 的模組不能直接或間接再 import 自己的型別**：那會無限遞迴，回報為循環 import。同一型別需要巢狀時，用 `Identity::key(..)` 區分。

- **`fn name()`** 決定錯誤訊息與 scope report 中的模組名稱，預設是型別名稱。

`hornero-config` 與 `hornero-sea-orm` 都是這種寫法的完整範例，見 [不用 macro](13-without-macros.md#自製整合-crate)。

## 模組層級的 middleware、layer 與 mount

模組也可以帶上只作用在自己 controller 上的東西：

- `middlewares = [M]` / `m.middleware::<M>()`：需要 DI 的 middleware。
- `m.layer(layer)`：任何 tower layer，例如只對 admin 模組加上 timeout。
- `m.mount_router(path, router)` / `m.mount(path, factory)`：掛進一個現成的 axum `Router`。

順序與差異見 [Request pipeline](05-request-pipeline.md)，掛載 axum router 見 [不用 macro](13-without-macros.md#l0掛進既有的-axum-router)。

## 組織應用的建議

- **依功能切模組**（`UsersModule`、`OrdersModule`），而不是依技術層切（`ServicesModule`、`RepositoriesModule`）。
- **exports 是模組的公開 API**。只 export 其他模組真正需要的 service，repository 與內部 helper 保持私有。
- **兩個模組互相需要時**，把共用的部分抽成第三個模組，讓雙方都 import 它。hornero 不支援循環 import，會回報 `circular module import` 並印出路徑。
- **根模組只負責組裝**：import 設定、資料庫與各功能模組，本身通常不提供 provider。
- **在測試中呼叫 `Hornero::new(AppModule).validate()`**，每次重構模組邊界後都能立刻知道有沒有東西斷掉。

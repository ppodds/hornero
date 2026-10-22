# hornero 設計文件

hornero 是建立在 tokio + axum 0.8 之上的 Web framework，提供 NestJS 式的分層抽象
（module / provider / controller / middleware / guard / interceptor / exception filter）與 DI，
但把 Nest 依賴 JS runtime reflection 的部分全部換成 Rust 慣用、能在啟動前驗證的設計。

重大決策各有一份 ADR：

| ADR | 決策 |
|---|---|
| [0001](adr/0001-runtime-di.md) | 執行期 TypeId 容器 + 啟動期整圖驗證，而非型別層級編譯期 DI |
| [0002](adr/0002-scope-bubbling.md) | Singleton / Request / Transient，request scope 採 Nest 式傳染，並以 scope report 與啟動期檢查控管 |
| [0003](adr/0003-strict-visibility.md) | 嚴格模組可見性：local > imports 的 exports > global |
| [0004](adr/0004-no-axum-wrapping.md) | 不包裝 axum：handler 參數就是 extractor，任何 tower layer 直接可用 |
| [0005](adr/0005-qualifiers.md) | 以 marker 型別作為 qualifier（`Pool@Primary`），並以 `Option<Arc<T>>` 表示可選依賴 |

---

## 1. 設計原則

1. **不包裝 axum。** facade 直接 re-export axum；框架只負責「組裝」。
2. **DI 成本集中在啟動期。** 依賴邊在啟動時解析成具體 node，singleton 在 request 期間零查表。
3. **Fail fast，錯誤訊息要好。** 缺依賴、循環、不可見、歧義、scope 違規、路由衝突，全部在 bind port 前一次回報，並附修正提示。
4. **漸進式。** 每個 macro 都只是公開 builder API 的語法糖。
5. **Types as tokens。** 沒有字串 token；同型別多實例用 qualifier 型別（`.qualified::<Replica>()`、`#[inject(qualifier = Replica)]`），或泛型 tag（`Db<Replica>`）、newtype。見 [ADR-0005](adr/0005-qualifiers.md)。

## 2. 漸進式採用

| 層級 | 寫法 | 範例 |
|---|---|---|
| L0 | 把既有 axum `Router` 掛進模組 | `m.mount_router("/legacy", router)`、`m.mount("/x", \|db: Arc<Db>\| async { ... })` |
| L1 | 手寫 `impl Module` / `impl Injectable` / `impl Controller` | `crates/hornero-core/tests/*.rs` 全部用 L1 寫成 |
| L2 | `#[module]`、`#[derive(Injectable)]`、`#[injectable]`、`#[controller]`、`#[routes]` | `crates/hornero/tests/macros.rs`、`examples/todo` |
| L3 | 整合 crate | `hornero-config`、`hornero-validation`、`hornero-sea-orm`、`hornero-openapi`、`hornero-testing` |

## 3. Crate 結構

```text
crates/
  hornero/              facade：re-export hornero-core、hornero-macros、axum、tower；prelude
  hornero-core/         DI 容器、模組系統、HTTP pipeline、lifecycle、錯誤型別
    src/di/          key、injectable、provider、module、graph（驗證）、runtime、scope、resolver
    src/web/         controller、handler、guard、interceptor、filter、middleware、enhancer、pipeline、route
    src/app.rs       Hornero builder、App、路由組裝、graceful shutdown
  hornero-macros/       proc macros（展開成 hornero-core 的公開 API）
  hornero-testing/      TestApp、override_*、in-process TestClient
  hornero-config/       ConfigModule（figment：toml + env + defaults）
  hornero-validation/   Valid<E> extractor（garde）
  hornero-sea-orm/      SeaOrmModule、Db<Tag>
  hornero-openapi/      OpenApiModule、document、ApiSecurity（utoipa + Swagger UI）
examples/todo/       完整範例 + e2e 測試
docs/               指南與 API 參考（hornero-docs crate：程式碼區塊會被編譯，或與真實檔案比對，或附上不編譯的理由）
```

> 計畫書中的 `http/` 目錄改名為 `web/`，避免與 `http` crate 在 `use http::...` 時撞名。

## 4. Provider 與 Module

### 4.1 Provider

所有註冊方式最後都產生一個 `ProviderDef`：key、依賴 key 清單、宣告的 scope、type-erased async factory。

| Nest | hornero |
|---|---|
| class provider | `m.provide::<T>()`，`T: Injectable`（`#[derive(Injectable)]` 或 `#[injectable] impl T { fn new(..) }`） |
| `useValue` | `m.provide_value(v)` / `value(expr)` |
| `useFactory`（含 async、inject） | `m.factory(\|a: Arc<A>\| async move { Ok::<_, BoxError>(..) })` / `factory(expr)` |
| `{ provide: TOKEN, useClass }` | `bind!(dyn Trait => Impl)` |
| `@Inject(TOKEN)` | 欄位型別 `Arc<dyn Trait>` 本身就是 token |

- `#[derive(Injectable)]`：每個 `Arc<T>` 欄位是一個依賴；其他欄位需要 `#[inject(default)]` 或 `#[inject(with = expr)]`，否則編譯錯誤並指出欄位。
- `#[injectable]` 放在 impl 上：使用 `new`（或標 `#[constructor]` 的函式）；可 async、可回傳 `Result<Self, E>`。
- `bind!` 需要 macro 的原因：stable Rust 無法在泛型裡把 `Arc<Impl>` unsize 成 `Arc<dyn Trait>`。
- 手寫 `Injectable` 時，`Resolver` 只包含宣告過的依賴；要求未宣告的型別會得到錯誤，而不是偷偷取用容器。

### 4.2 Module

<!-- ignore: 設計草圖：用到的型別都是示意，完整可編譯的版本見 examples/todo -->
```rust,ignore
#[module(
    imports = [AuthModule, ConfigModule::for_root(source).load::<AppConfig>()],
    controllers = [TodoController],
    providers = [TodoService, bind!(dyn TodoRepo => SeaOrmTodoRepo), value(AuditTrail::default())],
    exports = [TodoService, module(AuthModule)],
    middlewares = [RequestIdMiddleware],
    global,
)]
pub struct TodoModule;
```

- 動態模組（Nest 的 `forRoot`）就是帶設定的 struct 值；`Module::define(&self)` 只描述模組，I/O 放在 provider factory 裡（`validate()` 與 `build()` 都會呼叫 `define`）。
- 模組身分預設為 `TypeId`，可覆寫 `id()`（`ModuleId::with_discriminator`）；重複 import 會 dedup 成同一實例。需要「每次呼叫都是獨立實例」的動態模組（`ConfigModule::for_root`、`SeaOrmModule::for_root`）每個實例有唯一的 id，不會被 dedup；同時註冊兩個，下游就會得到歧義錯誤。
- 可見性規則見 ADR-0003。

### 4.3 Enhancer 也是 provider

`#[guards(AuthGuard)]` 會在 controller 所屬模組建立一個 *slot* node，slot 依賴 `AuthGuard`：

1. 若 `AuthGuard` 在該模組可見（例如 `AuthModule` export 了它），使用該實例，其依賴在 `AuthModule` 內解析；
2. 否則在該模組建立一個私有的隱式 provider（Nest 對 `@UseGuards(X)` 的行為相同）。

兩個可見來源都滿足 `AuthGuard` 時（例如同時 import 了兩個都 export 它的模組），和一般依賴一樣是 `AmbiguousDependency` 啟動錯誤，不會任選一個；模組 `exports` 轉出的型別有多個來源時也是，`export_module` 轉出的幾個模組提供不同實例時則是 `AmbiguousReexport`。

slot 以 `Arc<dyn ErasedGuard>` 儲存，因此 `override_guard::<AuthGuard>(AllowAll)` 可以換成任何 `Guard` 實作，且原本 guard 的依賴不再需要。

## 5. Scope

詳見 ADR-0002。摘要：

- `Scope::{Singleton, Request, Transient}`；effective scope = 自己宣告為 Request，或任一依賴的 effective scope 為 Request → Request。
- 啟動期限制：middleware、mount 不得變成 request-scoped；有 lifecycle hooks 的 provider 必須是 singleton；明確宣告 scope 的 provider（`#[injectable(scope = singleton)]`）被傳染時報錯。
- 啟動時以 `tracing::info!` 輸出 scope report：

```text
request-scoped components:
  AuditLog (in TodoModule): declared request-scoped
  TodoService (in TodoModule): request-scoped via TodoService -> AuditLog [request]
  TodoController (in TodoModule): request-scoped via TodoController -> TodoService -> AuditLog [request]
```

### Runtime 機制

- 只有 route 鏈上確實有 request-scoped 元件時，才會加上 scope layer，在 extensions 放入 `RequestScope`；純 singleton 的路由沒有任何額外成本。
- `RequestScope` 內部是 `Box<[tokio::sync::OnceCell<Instance>]>`，slot index 在啟動時算好；同一 request 內共用、跨 request 隔離。
- `Handle<T>` = `Ready(instance) | Lazy(node)`：controller / guard / interceptor / filter 都透過它取得實例；singleton 路徑只是一次 `Arc` clone。
- `RequestContext`（Nest 的 `REQUEST`）是 request head 的快照，每個元件看到的是**它被建構當下**的 request：快照在 request-scoped 的 guard、interceptor 或 handler 建構前建立，並在每個 guard 與 interceptor 之後作廢（它們可以修改 request），下一個需要的元件會重新拍一份。已建構的元件保留自己拿到的快照。因此 guard 寫入 extensions 的資料（例如 `AuthUser`）對之後建構的 request-scoped provider 可見，即使更早還有別的 request-scoped guard 用過 `RequestContext`。只有 exception filter 需要 context 時，由最先執行的階段拍一份（interceptor 提前回應時使用），guard 拒絕請求時重拍，到 handler 時若期間有 guard 或 interceptor 執行過也會重拍；filter 在 handler 之後執行，使用當時最新的快照，因此看得到 guard 寫入的資料。快照會移除 scope 本身，避免 `Arc` 循環。

## 6. Request pipeline

```text
request
  → 全域 tower layers / DI middleware（Hornero::layer / Hornero::middleware）
  → 模組 tower layers / DI middleware（只包住該模組的 controller）
  → routing
  → [scope layer]（僅在需要時）
  → exception filter（handler > controller > global 中，第一個 catches 該錯誤的）
  → guards（global → controller → handler，依序；第一個失敗即回應）
  → interceptors（global 包 controller 包 handler）
  → extractors（≈ Nest 的 Pipe；Valid<Json<T>> 做驗證）
  → handler（&self + extractors）
```

全部編譯成 `axum::middleware::from_fn` 或 tower layer，沒有另一套執行模型。`CatchPanicLayer` 在全域 middleware 與 tower layer 的內側、路由的外側作為保險（所以 panic 的 500 仍會經過 CORS、request id、trace 等全域處理，404 fallback 的 panic 也接得住），未匹配路由回 problem+json 404。應用的 fallback 只由全域 middleware 與 layer 包住，不經過任何模組的 middleware 或 layer：axum `merge` 留下的 fallback 是最後合併的模組的，帶著該模組的 layer，所以 hornero 另外組 fallback。掛在 `/` 的 router 自己的 fallback（例如 SPA 的 `index.html`）會被使用，只有 axum 空的 404 才換成 problem+json；這些 router 的路由在 fallback 裡一律回 404，免得全域 layer 在路由之後改寫路徑，繞過模組的 middleware 打到它們。mount 在非 `/` 前綴的 router 自己的 fallback 不在此列：axum `nest` 把它收在該前綴下，跟其他路由一樣經過模組的 middleware 與 layer。

route 層（scope、filter、guard、interceptor）以 `route_layer` 掛載，只對該 route 註冊的 HTTP method 執行：同一路徑上其他 method 的請求得到 405 時，不會執行任何 guard、interceptor 或 filter。405 同樣以 problem+json 回應，並保留 axum 設定的 `Allow` header。

| 抽象 | 簽名 | 說明 |
|---|---|---|
| `Guard` | `can_activate(&self, &mut GuardContext) -> Result<(), HttpError>` | 回傳 `Result` 而非 bool：自選 401/403；可寫入 `parts.extensions` |
| `Interceptor` | `intercept(&self, &RouteContext, Request, Next) -> Response` | RxJS Observable 換成 async around-advice |
| `ExceptionFilter` | `catches(&self, &HttpError) -> bool`（預設 `true`）與 `catch(&self, &HttpError, &RouteContext) -> Response` | 看得到 handler、guard、request-scoped 建構失敗產生的 `HttpError`；`?` 拋出的 domain error 以 `err.is::<T>()` 依型別接住 |
| `Middleware` | `handle(&self, Request, Next) -> Response` | 需要 DI 時才用；否則直接用 tower layer |

`RouteContext::meta::<T>()` 提供型別化 metadata（handler 層覆蓋 controller 層），取代 `SetMetadata` + `Reflector` 的字串 key。

**全域 enhancer 的兩種註冊方式。** `Hornero::guard::<G>()` 對應 `app.useGlobalGuards()`，在根模組解析。對應 `{ provide: APP_GUARD, useClass: G }` 的是模組 `providers` 裡的 token 型別 `AppGuard<G>`（以及 `AppInterceptor<I>`、`AppFilter<F>`），不另開 `#[module]` 選項：

- Nest 靠 multi-provider 讓多個 `APP_GUARD` 共用一個 token；hornero 的 key 是型別，同一個 key 註冊兩次是 `DuplicateProvider`。把 guard 型別放進 token 的泛型參數，每個 `AppGuard<X>` 就是不同的 key，不需要 multi-provider。
- `ModuleDef::provide` 接受 `T: Provide`：`impl<T: Injectable> Provide for T` 註冊 class provider，三個 token 各自有 `Provide` impl，把 enhancer 記到模組上。trait 與 token 都在 hornero-core，下游 crate 無法再為這些 token 實作 `Injectable`，所以兩組 impl 不會重疊。
- token 在宣告的模組解析（和 Nest 一樣，可以依賴該模組私有的 provider），解析規則與 `#[guards(G)]` 相同：模組看得到 `G` 就用那個實例，否則在該模組建立私有的隱式實例。
- 順序：`Hornero` 上註冊的先，接著是模組宣告的，被 import 的模組先於 import 它的模組（模組樹的後序），同一模組依宣告順序。Nest 的 `APP_GUARD` 順序取決於模組初始化順序，hornero 把它固定下來。

## 7. 錯誤模型

- Handler 回傳 `Result<_, HttpError>`。任何 `std::error::Error` 透過 `?` 轉成 500，原始錯誤保留為 source，**不會**送給 client。
- Domain error 的轉換交給 exception filter：`catches` 以 `err.is::<TodoError>()` 依 source 的型別選擇 filter（Nest 的 `@Catch(Type)`），沒有 filter 接住就維持 500。因此保留 `impl<E: Error> From<E> for HttpError`：它讓 `?` 能用在任何錯誤上，代價是使用者無法替自己的錯誤型別寫 `From<E> for HttpError`（E0119）；有了依型別選擇的 filter，就不需要那個 `From`。
- `HttpError` 預設渲染成 RFC 9457 `application/problem+json`，並把自己放進 response extensions 供 filter 重新渲染。它也攜帶回應 header（`with_header` / `headers()`，`unauthorized()` 預設帶 `WWW-Authenticate: Bearer`）：`render()` 會套用，自訂 filter 的回應則完全取代原回應，要保留就自己複製 `err.headers()`。`with_details` 序列化失敗時 `details` 維持空，並以 `tracing::warn!` 記錄。
- 5xx 的 log 寫在 router 最外層（所有全域 layer 之外），依**最終回應**決定：被 filter 轉成 4xx 的 domain error 不會留下 error log，panic 與沒被接住的錯誤各記錄一次。
- `hornero-validation` 把 extractor rejection 也轉成 `HttpError`，讓所有 client error 外觀一致、也會經過 filter。
- 啟動錯誤是 `BootError`，一次列出所有問題，例如：

```text
failed to bootstrap application: 1 problem

[1] missing dependency
    UserService (in UsersModule) requires `Database`, but no provider of it is visible there.
    hint: `Database` is provided by PrivateDatabaseModule but not exported; add it to the exports of PrivateDatabaseModule and import PrivateDatabaseModule in UsersModule.
```

所有錯誤訊息都有 insta snapshot（`crates/hornero-core/tests/snapshots`）。

## 8. Lifecycle 與關機

- Hooks：`on_module_init` → `on_application_bootstrap`（依拓撲序），`on_module_destroy` → `on_application_shutdown`（拓撲反序）。
- Rust 沒有 specialization，無法偵測 provider 是否實作 `Lifecycle`，所以以 `#[inject(hooks)]` / `#[injectable(hooks)]` / `ProviderDef::with_hooks::<T>()` 明確宣告；`with_hooks` 的型別參數和 provider 的型別不符時，是啟動時的 `Problem::InvalidProvider`，不是 panic。
- 啟動順序：驗證 → 建構 singleton → 組裝 router → `on_module_init` → `on_application_bootstrap`。router 先於 init hook 組裝，所以路由衝突在任何 hook 做 I/O 之前就被發現。
- 啟動失敗會清理：建構之後任何一步失敗，`build` 先觸發 `Shutdown`，再對已建構的有 hook 的 singleton 依反向依賴順序執行 destroy 與 shutdown hook（包括 init 還沒跑的），清理失敗接在原本的 `BootError` 之後。
- `App::close()` 把 destroy / shutdown hook 的失敗收集成 `ShutdownError`，與啟動失敗的 `BootError` 分開。`serve` 為 handler 提供 `ConnectInfo<SocketAddr>`；`hornero-testing` 的 request 帶有 loopback 的 peer 位址（`TestRequest::peer_addr` 可改）。
- `Drop` 不能 await，需要 I/O 的清理放在 hook 裡；內建可注入的 `Arc<Shutdown>`（包 `CancellationToken`）供背景任務監聽。
- `App::serve` 處理 SIGINT/SIGTERM 與 `Shutdown::trigger()`：graceful shutdown 後執行 destroy hooks；即使某個 hook 失敗也會跑完其他 hook。
- 等待進行中 request 有上限（`Hornero::shutdown_timeout`，預設 10 秒）：逾時或收到第二次 SIGINT/SIGTERM 就取消仍在執行的 request（還沒回應的 handler 被 drop、client 收到 503，仍在串流的 body 被切斷），等這些連線關閉（最多 1 秒）後照常執行 destroy hooks。axum 為每條連線 spawn 獨立的 task，只 drop serve 的 future 停不了它們，所以 `serve` 給每條連線一個共同 token 的 child token：request 只等自己連線的 token，不同連線的 request 不會搶同一把鎖。503 在全域 middleware 與 layer 的內側產生，所以和其他錯誤一樣帶著 CORS header、以 problem+json 回應、被 5xx log 記錄；卡在 `next` 之前的全域 middleware 由連線層再取消一次。限制：handler 自己 spawn 的 task（包括 upgrade 之後的 WebSocket 連線）不是 request，要自己監聽 `Shutdown`；不讀回應的 client 會讓 hyper 停止 poll body，這種連線在 1 秒後仍會留著，destroy hooks 照常執行。tokio 的訊號處理會取代預設的 Ctrl-C 終止行為，沒有這個機制時，一個卡住的 SSE 或慢 request 會讓第二次 Ctrl-C 也無效。
- `listen` 在 `build` 之後才綁定位址，因為 port 一開就會通過 TCP health check。綁定失敗（例如 port 已被占用）時會先 `close`，執行關閉 hook，再回傳錯誤，init hook 啟動的東西不會遺留；關閉 hook 也失敗時，錯誤的 `source()` 是 `ShutdownError`。

## 9. 測試

<!-- ignore: 節錄自 examples/todo 的測試，`todo::hornero()` 與 `InMemoryRepo` 屬於該範例 -->
```rust,ignore
let app = TestApp::from_hornero(todo::hornero())
    .override_value(DatabaseConfig { url: "sqlite::memory:".into(), max_connections: 1 })
    .override_provider::<dyn TodoRepo>(Arc::new(InMemoryRepo::default()))
    .override_guard::<AuthGuard>(AllowAll)
    .build()
    .await?;

let todo: Todo = app.client().with_bearer("t").post("/todos").json(&body).await
    .assert_status(StatusCode::CREATED)
    .json();
```

- override 會取代該 key 的所有 provider，且原 provider 的依賴不再需要；同一個 key 有多個 override 時，最後註冊的生效，被取代的不算「沒用到」；註冊了卻沒用到的 override 是啟動錯誤。
- `TestApp::get::<T>()` 是 service locator，只在測試開放。
- `#[test] fn graph() { hornero().validate().unwrap(); }` 不連資料庫、不建任何 provider，可以在 CI 擋下接線錯誤。路由衝突以 dry-run 組裝 router 檢查（controller 路由用佔位 handler，`mount_router` 用真正的 router，與 `build` 共用同一段組裝程式），所以 axum 會 panic 的衝突在 `validate()` 就找得到；只有需要 DI 的 `mount(..)` factory 要等到 `build`。axum 的 panic 以 `catch_unwind` 接住並轉成 `InvalidRoute`，預設的 panic hook 仍會把訊息印到 stderr（不為此替換全域 hook）。

## 10. 整合 crate

| crate | 提供 | 備註 |
|---|---|---|
| `hornero-config` | `ConfigModule::for_root(Source::new().toml(..).env("APP_")).load::<T>().section::<U>("key")` | `for_root` 在根模組註冊一次，預設 global；feature 模組用 `ConfigModule::for_feature().section::<T>(key)` 取自己的型別（非 global）；`__` 分隔巢狀 env key；設定錯誤是啟動錯誤 |
| `hornero-validation` | `Valid<Json<T>>`、`Valid<Query<T>>`、`Valid<Form<T>>`、`Valid<Path<T>>` | garde derive 需要應用程式直接依賴 `garde` |
| `hornero-sea-orm` | `SeaOrmModule::for_root(opts)` / `for_root_with(factory)` / `.tag::<Replica>()`，提供 `Arc<Db<Tag>>` | 預設 global；每個實例有唯一的模組 id，同一個 tag 註冊兩次是歧義錯誤；shutdown 時關閉連線；不做 `forFeature` |
| `hornero-testing` | `TestApp`、`TestClient`、`TestResponse` | 走完整 pipeline，不 bind port |
| `hornero-openapi` | `OpenApiModule::for_root(OpenApiConfig::new(..))`、`document(&routes, &config)`、`ApiSecurity` | 從 `Routes` 與 `#[routes]` 推導的 `RouteDoc` metadata 產生文件；型別能力以 autoref specialization 判斷，缺少 `ToSchema` 是 warning 而非編譯錯誤；見 [ADR-0006](adr/0006-openapi-inference.md) |

## 11. Nest 習慣 → Rust 替代方案

| Nest 習慣 | 在 Rust 的問題 | hornero 的做法 |
|---|---|---|
| reflect-metadata 執行期 DI、字串 token | 沒有 reflection；字串無型別 | TypeId 當 key、啟動期整圖驗證 |
| 每個 service 都抽 interface | dyn dispatch + async trait boxing，噪音多 | 預設注入具體型別；只在邊界（repo、外部 client）用 `Arc<dyn Trait>` |
| `forwardRef` | `Arc` 循環會洩漏，建構順序不存在 | 啟動錯誤並印出循環路徑 |
| Request scope 靜默傳染 | 隱性效能懸崖 | 保留傳染語意，但有 scope report 與啟動期檢查 |
| `throw HttpException` | 沒有例外 | `Result` + `?` + `HttpError` + filter |
| Pipes | — | extractor / newtype：parse, don't validate |
| Guard 回 bool + 字串 metadata | 無型別 | `Result<(), HttpError>` + 型別化 metadata + 證明型 extractor |
| `ModuleRef` / service locator | 依賴隱藏 | 依賴寫在建構子與 handler 簽名；locator 只在測試 |
| service 上的可變欄位 | 跨 task 共享 | 方法只吃 `&self`；狀態用 `Mutex` / atomic 明示 |
| 注入 Logger | — | `tracing` |
| `configService.get('KEY')` | 無型別 | serde typed config 注入 |
| CLS 隱式交易 | `tokio::spawn` 後失效 | 明確傳遞 transaction |
| 自動掃描註冊（`inventory` / `ctor`） | life-before-main、linker 可能丟棄 | 明確模組樹 |
| 重度 proc-macro DSL | 錯誤訊息差、IDE 體驗差 | macro 只做薄糖衣；trybuild 鎖定錯誤訊息 |

## 12. 已知限制與後續

- **自訂 HTTP method**（如 `PURGE`）：axum 的 `MethodFilter` 不支援，會在啟動時回報 `invalid route`。`#[route("METHOD", ..)]` 的 method 字串本身若不是合法的 token（例如含空白），則在編譯期就被拒絕。
- **路由 pattern 級 middleware**（Nest 的 `consumer.apply(..).forRoutes(..)`）：目前 middleware 以模組或全域為單位。
- **dyn async trait**：Rust 尚不支援 `async fn` 的 dyn dispatch，以 `BoxFuture` 回傳（見 `examples/todo/src/todo/repo.rs`）。
- **型別名稱**：診斷訊息使用 `std::any::type_name`，其格式不保證穩定（例如目前會省略預設泛型參數，`Db<Primary>` 顯示為 `Db`）。
- **sea-orm 驅動**：`sqlx-postgres` / `sqlx-mysql` 0.9 目前依賴尚未正式發布的 `hmac` / `sha1` 版本，因此 `hornero-sea-orm` 只提供 `sqlite` feature；其他驅動由應用程式自己的 `sea-orm` 依賴啟用。
- **後續**：`hornero-openapi`（utoipa）、排程、typed events、request-scoped transaction interceptor、無相依 singleton 平行建構。

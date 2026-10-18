# 15. 從 NestJS 過來

hornero 保留了 Nest 的架構（模組、provider、controller、enhancer、scope），但把 Nest 依賴 JavaScript 執行期反射的做法，換成 Rust 慣用、能在啟動前驗證的設計。

## 概念對照

### 模組與 DI

| NestJS | hornero | 說明 |
|---|---|---|
| `@Module({ imports, providers, controllers, exports })` | `#[module(imports = [..], providers = [..], controllers = [..], exports = [..])]` | 相同的嚴格可見性 |
| `@Global()` | `#[module(global, ..)]` | |
| `@Injectable()` | `#[derive(Injectable)]` 或 `#[injectable] impl` | |
| `constructor(private users: UsersService)` | 欄位 `users: Arc<UsersService>`，或建構函式參數 | |
| `@Inject('TOKEN')`、字串 token | qualifier：註冊時 `.qualified::<Token>()`，注入時 `#[inject(qualifier = Token)]` 或 `Qualified<T, Token>` | qualifier 是 marker 型別，不是字串；見 [同一個型別要多個實例](02-providers.md#同一個型別要多個實例) |
| `@Optional()` | `Option<Arc<T>>` 欄位或參數 | 看不到任何 provider 時是 `None`；歧義仍是錯誤 |
| `{ provide: Repo, useClass: PgRepo }` | `bind!(dyn Repo => PgRepo)` | |
| `useValue` | `value(expr)` | |
| `useFactory` + `inject: [...]` | `factory(\|a: Arc<A>\| async move { .. })` | 依賴直接寫在參數型別上 |
| `useExisting` | `ProviderDef::from_fn`，見 [不用 macro](13-without-macros.md#providerdef) | |
| `forRoot(options)` | 帶設定的 struct，例如 `CacheModule::new(1024)` | 見 [動態模組](03-modules.md#動態模組) |
| `forRootAsync({ useFactory, inject })` | `XxxModule::for_root_with(\|cfg: Arc<Cfg>\| async move { .. })` | |
| `forwardRef()` | 不支援；循環依賴是 bootstrap 錯誤 | 把共用部分抽成新的 provider |
| `ModuleRef.get()` / `app.get()` | `App::get::<T>()`、`App::get_qualified::<T, Q>()`，只給測試與啟動程式碼使用 | 一般程式碼請宣告依賴 |
| `Scope.REQUEST` / `Scope.TRANSIENT` | `#[inject(scope = request)]` / `scope = transient` | 同樣會傳染，另外有 scope report |
| `@Inject(REQUEST)` | `Arc<RequestContext>` | request head 的快照 |
| `OnModuleInit` 等介面 | `impl Lifecycle`，加上 `#[inject(hooks)]` 啟用 | 只支援 singleton |
| `app.enableShutdownHooks()` | 總是啟用（`listen` / `serve`） | |

### HTTP

| NestJS | hornero | 說明 |
|---|---|---|
| `@Controller('todos')` | `#[controller("/todos")]` | |
| `@Get(':id')` | `#[get("/{id}")]` | axum 0.8 的路徑語法 |
| `@Param('id')` | `Path(id): Path<u64>` | 型別就是解析器 |
| `@Query()` | `Query(q): Query<ListQuery>` | |
| `@Body()` | `Json(body): Json<CreateTodo>` | |
| `@Headers()`、`@Req()` | `HeaderMap`、任何 axum extractor | |
| `@HttpCode(201)` | 回傳 `(StatusCode::CREATED, Json(..))` | |
| `ParseIntPipe` 等 pipe | extractor 的型別（`Path<u64>`） | 解析失敗就拒絕 |
| `ValidationPipe` + class-validator | `Valid<Json<T>>` + garde | 見 [驗證](11-validation.md) |
| `CanActivate`（回傳 `boolean`） | `Guard`（回傳 `Result<(), HttpError>`） | guard 自己決定 401 還是 403 |
| `@UseGuards(AuthGuard)` | `#[guards(AuthGuard)]` | controller 層級寫在 `#[controller]` 下方，handler 層級寫在路由方法上 |
| `@UseInterceptors(Timing)` / `@UseFilters(F)` | `#[interceptors(Timing)]` / `#[filters(F)]` | 同上 |
| `app.useGlobalGuards()` / `useGlobalInterceptors()` / `useGlobalFilters()` | `Hornero::guard::<G>()` / `Hornero::interceptor::<I>()` / `Hornero::filter::<F>()` | 在根模組解析 |
| `{ provide: APP_GUARD, useClass: G }` | `providers = [AppGuard<G>]` | 在宣告的模組解析，可以依賴該模組私有的 provider；`Hornero::guard` 註冊的先執行，被 import 的模組先於 import 它的模組 |
| `APP_INTERCEPTOR` / `APP_FILTER` | `AppInterceptor<I>` / `AppFilter<F>` | 同上；`Hornero::filter` 註冊的 filter 先比對 |
| `@Roles('admin')`（`SetMetadata` + `Reflector`） | `#[meta(Roles(&["admin"]))]`，讀取 `ctx.route.meta::<Roles>()` | 以型別為 key，不需要自訂 decorator |
| `NestInterceptor`（RxJS） | `Interceptor`：`async fn(ctx, req, next) -> Response` | |
| `HttpException` | `HttpError` | 預設渲染成 problem+json |
| `@Catch(TodoError)` + `ExceptionFilter` | `ExceptionFilter`，`catches` 回傳 `err.is::<TodoError>()` | 同一層裡先註冊的先比對，所以具體的 filter 要寫在 catch-all 前面（Nest 相反） |
| `throw new TodoNotFound()` | `return Err(TodoError::NotFound(id))`，或在 handler 裡 `?` | 沒有 filter 接住就是 500 |
| `NestMiddleware` + `configure(consumer)` | `Middleware` + `middlewares = [..]`，或 tower layer | 不需要 DI 時直接用 tower-http |
| `app.setGlobalPrefix('api')` | `Hornero::global_prefix("/api")` | |

### 生態系

| NestJS | hornero |
|---|---|
| `@nestjs/config` 的 `ConfigModule.forRoot()` / `forFeature()` 與 `ConfigService.get('KEY')` | `hornero-config`：`ConfigModule::for_root` / `for_feature`，型別化的 `Arc<AppConfig>` |
| `TypeOrmModule.forRoot` / `forFeature` | `hornero-sea-orm`：`SeaOrmModule::for_root`，沒有 `forFeature` |
| `@nestjs/swagger` 的 `SwaggerModule.setup` 與 `@ApiResponse()` 等 decorator | `hornero-openapi`：`OpenApiModule::for_root`，參數與 body 從 handler 簽名推導，`#[api_response]` 補充；見 [OpenAPI](16-openapi.md#對照-nestjsswagger) |
| `@nestjs/testing` 的 `Test.createTestingModule().overrideProvider()` | `hornero-testing`：`TestApp::new(..).override_provider::<T>(..)`；帶 qualifier 的 provider 用 `override_with(ProviderDef::value(fake).qualified::<Q>())` |
| supertest | `TestApp::client()`（in-process，不開 port） |

## 主要差異

- **錯誤在啟動前出現**：缺少的依賴、不可見的 provider、循環、scope 違規、路由衝突，都會在 bind port 之前一次列出。`Hornero::validate()` 可以放進單元測試。
- **衝突一律是錯誤**：同一模組重複註冊同一個 provider（Nest 會讓後者覆蓋前者），或兩個 import 都 export 同一個 provider（Nest 依 import 順序挑一個），在 hornero 都是 bootstrap 錯誤。
- **沒有反射**：provider 的依賴寫在型別上（`Arc<T>` 欄位），metadata 以型別為 key。生命週期 hook 需要明確啟用，因為 Rust 沒有 specialization，hornero 無法自動發現 trait 實作。
- **沒有 exception**：handler 回傳 `Result<_, HttpError>`，`?` 會把任何錯誤變成不透露細節的 500，原始錯誤保留下來，讓 exception filter 依型別轉成其他回應。
- **request 期間沒有 DI 成本**：singleton 在啟動時注入完畢，只有 request-scoped 鏈才會在每個 request 建構。
- **不包裝 axum**：extractor、`IntoResponse`、tower layer 都是 axum 原生的東西，可以隨時退回純 axum。

## FAQ

**為什麼不在編譯期檢查依賴圖？**
proc macro 一次只看得到一個 item，跨模組、跨 crate 的檢查只能交給 trait 系統，錯誤訊息會變成難以閱讀的 trait bound 錯誤，也無法依設定動態組裝模組。hornero 選擇在啟動時完整驗證，並提供 `validate()` 讓檢查在 `cargo test` 就跑完。詳見 [ADR-0001](../adr/0001-runtime-di.md)。

**為什麼依賴一定要是 `Arc<T>`？**
同一個實例會同時服務許多 request，可能在不同執行緒上，所以必須是共享、`Send + Sync` 的。需要可變狀態時，在內部使用 `Mutex`、`RwLock` 或 atomic。

**trait 的 async 方法怎麼放進 `Arc<dyn Trait>`？**
`async fn` 在 trait 裡無法透過 `dyn` 呼叫。讓方法回傳 `hornero::BoxFuture<'_, T>`，見 [Provider](02-providers.md#trait-object-與-async-方法)。

**可以用 axum 的 `State` 嗎？**
controller 不使用 `State`，依賴透過 DI 注入。mount 進來的 axum router 可以照常使用 `State`，見 [L0](13-without-macros.md#l0掛進既有的-axum-router)。

**深層的 service 要怎麼拿到目前的使用者？**
把它做成 request-scoped provider，從 `Arc<RequestContext>` 讀取 guard 寫入的資料，見 [Scope](06-scopes.md)。如果只有 handler 需要，用 extractor 更簡單，也不會傳染 scope。

**request 回應 `500 failed to construct a request-scoped component`？**
某個 request-scoped 元件在 request 期間建構失敗（例如從 `RequestContext` 找不到預期的資料）。真正的原因寫在 log 裡。

**我的 interceptor 沒有記錄到 401？**
interceptor 在 guard 裡面執行，被 guard 擋下的 request 不會經過 interceptor。請用 `TraceLayer` 或 middleware 記錄所有 request。

**自訂 exception filter 之後，401 少了 `WWW-Authenticate` header？**
filter 產生的回應會完全取代預設回應，而 header 是跟著 `HttpError` 走的。從 `err.render()` 開始修改，或自己組回應時把 `err.headers()` 複製進去（`response.headers_mut().extend(err.headers().clone())`）。

**可以用自訂的 HTTP method 嗎？**
axum 的路由無法比對自訂 method（例如 `PURGE`），hornero 會在啟動時回報 `invalid route`。

**錯誤訊息裡的 `Db<Primary>` 顯示成 `Db`？**
Rust 的 `std::any::type_name` 會省略預設的泛型參數。`Db` 就是 `Db<Primary>`。

**`hornero` 這個名字會改嗎？**
目前沒有計畫。名稱取自灶鳥（hornero，ovenbird）：牠用黏土築出像窯的巢，呼應 Nest 的「巢」。hornero 是實驗性專案，也還沒發佈到 crates.io，發佈前仍可能調整。

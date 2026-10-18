# `hornero-testing` API 參考

測試工具，相當於 `@nestjs/testing` 加上 supertest。request 在 process 內直接送進組好的 router（`tower::ServiceExt::oneshot`），所有 guard、interceptor、filter 與 middleware 都會執行，不會開 port。

```toml
[dev-dependencies]
hornero-testing = { path = "crates/hornero-testing" }
tokio = { version = "1", features = ["macros", "rt"] }
```

| 型別 | 用途 |
|---|---|
| [`TestApp`](#testapp) | 已啟動、可以測試的應用 |
| [`TestAppBuilder`](#testappbuilder) | 設定 override 與其他 `Hornero` 選項 |
| [`TestClient`](#testclient) | in-process 的 HTTP client |
| [`TestRequest`](#testrequest) | 建構中的 request，`.await` 就會送出 |
| [`TestResponse`](#testresponse) | 讀完 body 的回應 |

使用方式見 [測試](../guide/09-testing.md)。

---

## `TestApp`

```rust,ignore
pub struct TestApp { /* ... */ }
```

| 方法 | 說明 |
|---|---|
| `fn new(root: impl Module) -> TestAppBuilder` | 以根模組開始設定，相當於 `Test.createTestingModule` |
| `fn from_hornero(hornero: Hornero) -> TestAppBuilder` | 從現成的 `Hornero` builder 開始，例如 `main` 用的那個（含全域 guard、layer 等） |
| `fn client(&self) -> TestClient` | 新的 client，沒有預設 header |
| `fn get<T: ?Sized>(&self) -> Arc<T>` | 依型別取得 singleton；找不到時 panic，訊息會說明原因 |
| `fn try_get<T: ?Sized>(&self) -> Option<Arc<T>>` | 同上，找不到時回傳 `None` |
| `fn get_qualified<T: ?Sized, Q>(&self) -> Arc<T>` | 取得以 qualifier `Q` 註冊的 `T`；找不到時 panic |
| `fn scope_report(&self) -> &ScopeReport` | 哪些元件是 request-scoped，以及原因 |
| `fn app(&self) -> &App` | 底下的 `App` |
| `fn router(&self) -> Router` | 組好的 axum `Router`，可以用 `oneshot` 送任意 request |
| `async fn close(self) -> Result<(), ShutdownError>` | 執行關閉 hook（`on_module_destroy`、`on_application_shutdown`） |

`get`、`try_get` 與 `get_qualified` 只找得到 singleton：根模組看得到的那個，或整個應用唯一的那個。request-scoped 與 transient 的 provider 無法這樣取得。

---

## `TestAppBuilder`

```rust,ignore
pub struct TestAppBuilder { /* ... */ }
```

由 `TestApp::new` 或 `TestApp::from_hornero` 產生。每個方法都取得並回傳 `self`，對應 [`Hornero`](hornero-core.md#hornero) 的同名方法。

| 方法 | 說明 |
|---|---|
| `fn override_provider<T: ?Sized>(self, value: Arc<T>) -> Self` | 以 `value` 取代 `T` 的 provider，例如 `override_provider::<dyn Repo>(Arc::new(InMemoryRepo::default()))` |
| `fn override_value<T>(self, value: T) -> Self` | 同上，傳值 |
| `fn override_with(self, def: ProviderDef) -> Self` | 以任意 `ProviderDef` 取代，例如需要依賴其他 provider 的假物件，或帶 qualifier 的 provider：`override_with(ProviderDef::value(fake).qualified::<Primary>())` |
| `fn override_guard<G>(self, guard: impl Guard) -> Self` | 以 `guard` 取代所有用到 guard `G` 的地方 |
| `fn override_interceptor<I>(self, interceptor: impl Interceptor) -> Self` | 同上，interceptor |
| `fn override_filter<F>(self, filter: impl ExceptionFilter) -> Self` | 同上，exception filter |
| `fn configure(self, f: impl FnOnce(Hornero) -> Hornero) -> Self` | 任何其他 `Hornero` 設定：全域 enhancer、layer、`global_prefix` |
| `async fn build(self) -> Result<TestApp, BootError>` | 驗證並啟動應用 |

同一個 key 註冊多個 override 時，最後註冊的生效（可以在共用的 builder 上再疊一個 override），被取代的不會回報。override 掉的 provider 不會被建構，它原本的依賴也不再需要，所以可以只匯入要測的模組，再把它依賴的外部服務都換成假物件。override 一個沒有任何模組提供的型別，會得到 `Problem::UnusedOverride`。

替換的 guard、interceptor 或 filter 不需要實作 `Injectable`，可以是任何值，例如帶著測試資料的 struct。

---

## `TestClient`

```rust,ignore
#[derive(Clone)]
pub struct TestClient { /* ... */ }
```

| 方法 | 說明 |
|---|---|
| `fn with_header(self, name: &str, value: &str) -> Self` | 這個 client 送出的每個 request 都帶上這個 header |
| `fn with_bearer(self, token: &str) -> Self` | `with_header("authorization", "Bearer {token}")` |
| `fn request(&self, method: Method, path: &str) -> TestRequest` | 任意 method 的 request |
| `fn get` / `post` / `put` / `patch` / `delete(&self, path: &str) -> TestRequest` | `request` 的簡寫 |

`path` 可以包含 query string，例如 `"/todos?done=true"`。無效的路徑、header 名稱或 header 值會直接 panic，因為那是測試本身寫錯。

---

## `TestRequest`

```rust,ignore
pub struct TestRequest { /* ... */ }
impl IntoFuture for TestRequest { type Output = TestResponse; /* ... */ }
```

| 方法 | 說明 |
|---|---|
| `fn header(self, name: &str, value: &str) -> Self` | 加上 header（覆寫 client 的同名 header） |
| `fn bearer(self, token: &str) -> Self` | `header("authorization", "Bearer {token}")` |
| `fn peer_addr(self, addr: SocketAddr) -> Self` | 設定 handler 透過 `ConnectInfo<SocketAddr>` 看到的 client 位址；預設為 `127.0.0.1:40000` |
| `fn json<T: Serialize + ?Sized>(self, body: &T) -> Self` | JSON body，並設定 `content-type: application/json` |
| `fn body(self, body: impl Into<Body>) -> Self` | 任意 body，例如 `String`、`&'static str`、`Vec<u8>`；不會設定 `content-type` |
| `async fn send(self) -> TestResponse` | 送出 request 並讀完 body |

`TestRequest` 實作了 `IntoFuture`，所以 `client.get("/").await` 與 `client.get("/").send().await` 相同。

---

## `TestResponse`

```rust,ignore
#[derive(Debug)]
pub struct TestResponse { /* ... */ }
```

| 方法 | 說明 |
|---|---|
| `fn status(&self) -> StatusCode` | 狀態碼 |
| `fn headers(&self) -> &HeaderMap` | 所有 header |
| `fn header(&self, name: &str) -> Option<&str>` | 一個 header 的值；不存在或不是 ASCII 時為 `None` |
| `fn bytes(&self) -> &Bytes` | body 原始內容 |
| `fn text(&self) -> String` | body 轉成字串（無效的 UTF-8 會被替換） |
| `fn json<T: DeserializeOwned>(&self) -> T` | 反序列化 body；失敗時 panic，並印出 body |
| `fn assert_status(self, expected: StatusCode) -> Self` | 狀態碼不符時 panic，並印出 body；回傳 `self` 以便串接 |

---

## 範例

```rust
use hornero::prelude::*;
use hornero_testing::TestApp;
use serde_json::{Value, json};

trait Greetings: Send + Sync {
    fn word(&self) -> String;
}

#[derive(Injectable)]
struct English;

impl Greetings for English {
    fn word(&self) -> String {
        "hello".into()
    }
}

struct Pirate;

impl Greetings for Pirate {
    fn word(&self) -> String {
        "ahoy".into()
    }
}

#[derive(Injectable)]
struct TokenGuard;

impl Guard for TokenGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        match ctx.parts.headers.get("authorization") {
            Some(value) if value == "Bearer secret" => Ok(()),
            _ => Err(HttpError::unauthorized("missing token")),
        }
    }
}

#[controller("/greet")]
#[guards(TokenGuard)]
struct GreetController {
    greetings: Arc<dyn Greetings>,
}

#[routes]
impl GreetController {
    #[post("/")]
    fn greet(&self, Json(body): Json<Value>) -> Json<Value> {
        Json(json!({ "message": format!("{}, {}", self.greetings.word(), body["name"]) }))
    }
}

#[module(controllers = [GreetController], providers = [bind!(dyn Greetings => English)])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = TestApp::new(AppModule)
    .override_provider::<dyn Greetings>(Arc::new(Pirate))
    .build()
    .await
    .unwrap();

let client = app.client().with_bearer("secret");
let body: Value = client
    .post("/greet")
    .json(&json!({ "name": "ann" }))
    .await
    .assert_status(StatusCode::OK)
    .json();
assert_eq!(body["message"], r#"ahoy, "ann""#);

let res = app.client().post("/greet").json(&json!({})).await;
assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
assert_eq!(res.header("www-authenticate"), Some("Bearer"));

app.close().await.unwrap();
# }
```

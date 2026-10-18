# 1. 快速入門

本章在十分鐘內做出第一個 hornero 應用：一個 provider、一個 controller、一個模組，再加上測試。

## 需求

- Rust 1.88 以上（edition 2024）。
- 懂 axum 會有幫助，但不是必要：hornero 的 handler 參數就是 axum extractor，回傳值就是 axum 的 `IntoResponse`。

## 安裝

hornero 尚未發佈到 crates.io，目前請用 path 或 git 依賴：

```toml
[dependencies]
hornero = { path = "../hornero/crates/hornero" }            # 或 { git = "<repository url>" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tracing-subscriber = "0.3"                        # 選用：在終端機看到啟動 log

[dev-dependencies]
hornero-testing = { path = "../hornero/crates/hornero-testing" }
```

`hornero` 是 facade crate：它 re-export 了核心（`hornero-core`）、所有 macro，以及 `axum` 和 `tower`。要直接使用 axum 時，用 `hornero::axum` 或自己加 `axum = "0.8"` 都可以。

## 第一個應用

把以下內容放進 `src/main.rs`：

<!-- snippet: examples/hello.rs#app -->
```rust,ignore
use hornero::prelude::*;

#[derive(Injectable)]
struct Greeter;

impl Greeter {
    fn greet(&self, name: &str) -> String {
        format!("hello, {name}")
    }
}

#[controller("/hello")]
struct HelloController {
    greeter: Arc<Greeter>,
}

#[routes]
impl HelloController {
    #[get("/{name}")]
    async fn hello(&self, Path(name): Path<String>) -> String {
        self.greeter.greet(&name)
    }
}

#[module(controllers = [HelloController], providers = [Greeter])]
struct AppModule;
```

逐一說明：

- **`#[derive(Injectable)]`** 告訴容器如何建構 `Greeter`。它沒有欄位，所以沒有依賴。
- **`#[controller("/hello")]`** 宣告一個 controller，路由前綴是 `/hello`。controller 本身也是 provider：每個 `Arc<T>` 欄位都是依賴，在啟動時注入。
- **`#[routes]`** 把標了 `#[get]`、`#[post]` 等屬性的方法變成路由。`&self` 之後的參數都是 axum extractor，回傳值可以是任何 `IntoResponse`。
- **`#[module]`** 把 controller 和 provider 組成模組。provider 預設只在自己的模組裡可見。

接著是 `main`：

<!-- snippet: examples/hello.rs#main -->
```rust,ignore
#[tokio::main]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::fmt::init();
    Hornero::new(AppModule).listen("127.0.0.1:3000").await
}
```

執行它：

```sh
cargo run
curl localhost:3000/hello/hornero      # hello, hornero
curl -i localhost:3000/nope         # 404，Content-Type: application/problem+json
```

在 hornero 的 repository 裡，這個範例就是 `docs/examples/hello.rs`，可以直接執行：`cargo run -p hornero-docs --example hello`。

## 啟動時發生了什麼

`listen` 會依序做這些事：

1. **走訪模組樹**：從 `AppModule` 出發，收集所有 import 的模組，重複的模組只算一次。
2. **驗證整張依賴圖**：缺少的依賴、循環依賴、不可見的 provider、歧義、scope 規則、路由衝突。所有問題會一次列出，並在 bind port 之前就失敗。
3. **建構 singleton**：依拓撲順序建構（先 `Greeter`，再 `HelloController`），然後執行生命週期 hook。
4. **組出 axum `Router`** 並開始 listen。收到 Ctrl-C 或 SIGTERM 時會 graceful shutdown，接著執行關閉 hook。

request 進來時不會再查詢容器：controller 的 `Arc` 在啟動時就準備好了。DI 的成本全部集中在啟動期。

## 故意弄壞它

把 `providers = [Greeter]` 拿掉，hornero 在啟動時就會拒絕這個應用。`Hornero::validate()` 只檢查依賴圖與路由、不建構任何東西，很適合拿來示範：

```rust
use hornero::prelude::*;

#[derive(Injectable)]
struct Greeter;

#[controller("/hello")]
struct HelloController {
    greeter: Arc<Greeter>,
}

#[routes]
impl HelloController {}

#[module(controllers = [HelloController])] // 忘了 providers = [Greeter]
struct AppModule;

let err = Hornero::new(AppModule).validate().unwrap_err();
assert_eq!(
    err.to_string(),
    "failed to bootstrap application: 1 problem

[1] missing dependency
    HelloController (in AppModule) requires `Greeter`, but no provider of it is visible there.
    hint: no module provides `Greeter`; register it in the providers of AppModule or of a module AppModule imports.
"
);
```

錯誤訊息會指出是誰需要什麼、在哪個模組，並在 hornero 猜得到修正方式時附上提示。常見錯誤與修正方式見 [錯誤處理](08-errors.md#bootstrap-錯誤)。

## 第一個測試

在 `main.rs` 最後加上測試：

<!-- snippet: examples/hello.rs#tests -->
```rust,ignore
#[cfg(test)]
mod tests {
    use super::*;
    use hornero_testing::TestApp;

    #[test]
    fn dependency_graph_is_valid() {
        Hornero::new(AppModule).validate().unwrap();
    }

    #[tokio::test]
    async fn says_hello() {
        let app = TestApp::new(AppModule).build().await.unwrap();

        let res = app.client().get("/hello/hornero").await;

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text(), "hello, hornero");
    }
}
```

- `dependency_graph_is_valid` 只做驗證，不建構任何東西，執行很快。建議每個專案都放一個。
- `TestApp` 會完整啟動應用，再用 in-process 的 client 直接呼叫 router，不需要開 port。guard、interceptor、filter 都會照常運作。細節見 [測試](09-testing.md)。

```sh
cargo test
```

## 下一步

- 想把應用拆成多個模組：[模組](03-modules.md)
- 想知道 provider 有哪些寫法：[Provider 與依賴注入](02-providers.md)
- 想加上驗證、權限與錯誤處理：[Request pipeline](05-request-pipeline.md)、[錯誤處理](08-errors.md)
- 想看一個完整的應用：[`examples/todo`](../../examples/todo/src/lib.rs)

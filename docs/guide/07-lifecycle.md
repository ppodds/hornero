# 7. 生命週期與關閉

## 生命週期 hook

`Lifecycle` trait 有四個 async hook，都有預設的空實作，只要覆寫需要的那幾個：

| Hook | 時機 | 順序 |
|---|---|---|
| `on_module_init` | 所有 singleton 都建構完成之後 | 依賴順序（被依賴的先） |
| `on_application_bootstrap` | 所有 `on_module_init` 完成之後、開始接受 request 之前 | 依賴順序 |
| `on_module_destroy` | 關閉時，server 停止接受 request 之後 | 反向依賴順序 |
| `on_application_shutdown` | 所有 `on_module_destroy` 完成之後 | 反向依賴順序 |

Rust 沒有 specialization，hornero 無法自動發現某個型別實作了 `Lifecycle`，所以必須明確啟用：

| Provider 寫法 | 啟用方式 |
|---|---|
| `#[derive(Injectable)]` | struct 上加 `#[inject(hooks)]` |
| `#[injectable]` | `#[injectable(hooks)]` |
| 手寫 `Injectable` | `const LIFECYCLE: Option<LifecycleFn<Self>> = hornero::hooks::<Self>();` |
| factory / value provider | `ProviderDef::factory(..).with_hooks::<T>()` |

```rust
use std::sync::Mutex;

use hornero::prelude::*;

#[derive(Default)]
struct Events(Mutex<Vec<&'static str>>);

impl Events {
    fn push(&self, event: &'static str) {
        self.0.lock().unwrap().push(event);
    }
}

#[derive(Injectable)]
#[inject(hooks)]
struct Database {
    events: Arc<Events>,
}

impl Lifecycle for Database {
    async fn on_module_init(&self) -> Result<(), BoxError> {
        self.events.push("database: init");
        Ok(())
    }

    async fn on_application_shutdown(&self) -> Result<(), BoxError> {
        self.events.push("database: shutdown");
        Ok(())
    }
}

#[derive(Injectable)]
#[inject(hooks)]
struct Cache {
    db: Arc<Database>,
    events: Arc<Events>,
}

impl Lifecycle for Cache {
    async fn on_module_init(&self) -> Result<(), BoxError> {
        self.events.push("cache: init"); // Database 的 init 已經跑完，可以安心使用它
        Ok(())
    }

    async fn on_application_bootstrap(&self) -> Result<(), BoxError> {
        self.events.push("cache: bootstrap");
        Ok(())
    }

    async fn on_module_destroy(&self) -> Result<(), BoxError> {
        self.events.push("cache: destroy");
        Ok(())
    }
}

#[module(providers = [value(Events::default()), Database, Cache])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap(); // init 與 bootstrap hook 在這裡執行
let events = app.get::<Events>().unwrap();
app.close().await.unwrap(); // destroy 與 shutdown hook 在這裡執行

assert_eq!(
    *events.0.lock().unwrap(),
    ["database: init", "cache: init", "cache: bootstrap", "cache: destroy", "database: shutdown"]
);
# }
```

### 規則

- **只有 singleton 能有 hook**。有 hook 的 provider 若是 request-scoped 或 transient（包括被傳染），啟動時會報錯。
- **init 或 bootstrap hook 失敗會中止啟動**，回傳 `BootError`：

  ```text
  [1] lifecycle hook failed
      Exploding (in App) failed in on_module_init: migrations failed
  ```

- **啟動失敗會先清理**。singleton 建構失敗、init 或 bootstrap hook 失敗、router 組裝失敗（路由衝突）時，`build` 會先觸發 `Shutdown`，再對**已經建構的每個有 hook 的 singleton**，依反向依賴順序執行 `on_module_destroy` 與 `on_application_shutdown`（包括 init 還沒執行的，例如 `Db` 的連線池也會關閉），讓 `on_module_init` 啟動的背景工作不會殘留。清理時 hook 的失敗會記錄到 log，並接在原本的問題之後放進同一個 `BootError`。
- router 在 `on_module_init` 之前組裝，所以路由衝突會在任何 hook 做 I/O 之前就被發現。
- **destroy 與 shutdown hook 一定會全部執行**，即使前面的失敗了。所有失敗會收集起來，由 `App::close()` 以 `ShutdownError`（`Display` 為 `failed to shut down application: …`）回傳，不會被誤認為啟動失敗。
- **`Drop` 不能 await**：關閉連線池、flush buffer 這類需要 I/O 的清理請放在 `on_module_destroy` 或 `on_application_shutdown`。

## Graceful shutdown

`Hornero::listen` 會處理整個流程：

1. 啟動並開始接受 request。
2. 收到 SIGINT（Ctrl-C）或 SIGTERM，或有人呼叫 `Shutdown::trigger()`。
3. 停止接受新連線，等待進行中的 request 完成（axum graceful shutdown）。最多等 `Hornero::shutdown_timeout`（預設 10 秒）；逾時，或再收到一次 SIGINT/SIGTERM，就取消仍在執行的 request（會記一筆 warning）：還沒回應的 handler 被 drop，client 收到 problem+json 的 503（會經過全域 middleware 與 layer，例如帶上 CORS header）；仍在串流的 body（例如 SSE）被切斷。這些連線關閉後（最多再等 1 秒）繼續下一步。
4. 依反向依賴順序執行 `on_module_destroy`，再執行 `on_application_shutdown`。
5. `listen` 回傳。

第 3 步只取消 request。handler 自己 spawn 的 task（包括 upgrade 之後的 WebSocket 連線）不在其中，請讓它們監聽 `Shutdown`（見下方的背景工作寫法），並在 `on_module_destroy` 等它們停下來。不讀回應的 client 也可能讓連線撐過那 1 秒，第 4 步照常執行。

位址無法綁定時（例如 port 已被占用），`listen` 會先執行第 4 步再回傳錯誤，`on_application_bootstrap` 啟動的背景任務不會遺留。回傳的仍是綁定的 `io::Error`（`kind()` 不變）；第 4 步的 hook 也失敗時，它的 `source()` 是 `ShutdownError`。

handler 可以用 `ConnectInfo<SocketAddr>` 取得 client 位址；`serve` 與 `listen` 都會提供。

等待時間可以調整，`Duration::MAX` 表示無限等待：

```rust,no_run
use std::time::Duration;

use hornero::prelude::*;

# #[module]
# struct AppModule;
# #[tokio::main]
# async fn main() -> Result<(), BoxError> {
Hornero::new(AppModule)
    .shutdown_timeout(Duration::from_secs(30))
    .listen("0.0.0.0:8080")
    .await?;
# Ok(())
# }
```

這個上限很重要：tokio 的訊號處理會取代 Ctrl-C 預設的終止行為，一個卡住的 SSE 串流或慢 request 會讓 graceful shutdown 一直等下去，而且再按一次 Ctrl-C 也沒有用，沒有上限的話只剩 `kill -9`。

要自己控制 listener 時，用 `build` 加 `serve`：

```rust,no_run
use hornero::prelude::*;
use tokio::net::TcpListener;

#[module]
struct AppModule;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let app = Hornero::new(AppModule).build().await?;
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    app.serve(listener).await // 和 listen 一樣：處理訊號、graceful shutdown、執行關閉 hook
}
```

只拿 `app.router()` 自己 serve（例如嵌入既有的 axum 應用）時，記得在結束時呼叫 `app.close().await`，關閉 hook 才會執行。

## `Shutdown`：背景工作與主動關閉

`Arc<Shutdown>` 是內建的 singleton，每個模組都可以注入：

| 方法 | 用途 |
|---|---|
| `cancelled().await` | 等到應用開始關閉 |
| `is_triggered()` | 是否已經開始關閉 |
| `trigger()` | 主動關閉應用（例如管理用的 endpoint 或致命錯誤） |
| `token()` | 取得一個會跟著應用一起取消的 `CancellationToken`，可以交給其他 library |

背景工作的典型寫法：在 `on_application_bootstrap` 啟動，等 `cancelled()` 結束，在 `on_module_destroy` 等它真的停下來：

```rust
use std::sync::Mutex;
use std::time::Duration;

use hornero::prelude::*;
use tokio::task::JoinHandle;

#[derive(Injectable)]
#[inject(hooks)]
struct Heartbeat {
    shutdown: Arc<Shutdown>,
    #[inject(default)]
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Lifecycle for Heartbeat {
    async fn on_application_bootstrap(&self) -> Result<(), BoxError> {
        let shutdown = self.shutdown.clone();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = ticker.tick() => tracing::debug!("still alive"),
                    _ = shutdown.cancelled() => break,
                }
            }
        });
        *self.task.lock().unwrap() = Some(task);
        Ok(())
    }

    async fn on_module_destroy(&self) -> Result<(), BoxError> {
        // 走到這裡時 Shutdown 已經被觸發，迴圈會自己結束；這裡只是等它真的結束
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            task.await?;
        }
        Ok(())
    }
}

#[module(providers = [Heartbeat])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
let shutdown = app.shutdown_handle();
app.close().await.unwrap();
assert!(shutdown.is_triggered());
# }
```

`App::close()` 會先觸發 `Shutdown`，再執行 destroy hook，所以背景工作總是會先收到取消訊號。

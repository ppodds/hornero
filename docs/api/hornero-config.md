# `hornero-config` API 參考

型別化的設定。以 [figment](https://docs.rs/figment) 讀取 TOML 檔與環境變數，反序列化成你自己的 serde struct，再以 `Arc<T>` 注入，取代 Nest 的 `configService.get('KEY')`。

```toml
[dependencies]
hornero-config = { path = "crates/hornero-config" }
serde = { version = "1", features = ["derive"] }
```

| 型別 | 用途 |
|---|---|
| [`Source`](#source) | 設定從哪裡來，後面的來源覆寫前面的 |
| [`ConfigModule`](#configmodule) | 提供並 export 每個載入的設定型別 |

使用方式見 [設定](../guide/10-configuration.md)。

---

## `Source`

```rust,ignore
#[derive(Clone, Default)]
pub struct Source { /* ... */ }
```

| 方法 | 說明 |
|---|---|
| `fn new() -> Source` | 空的來源 |
| `fn toml(self, path: impl AsRef<Path>) -> Self` | TOML 檔，從工作目錄往上層尋找；檔案不存在時略過（之後若載入失敗，錯誤訊息會註明該檔案沒找到） |
| `fn toml_required(self, path: impl AsRef<Path>) -> Self` | 同 `toml`，但檔案找不到時，啟動會失敗並在錯誤中指出檔名 |
| `fn toml_str(self, toml: &str) -> Self` | 內嵌的 TOML 字串，適合預設值與測試 |
| `fn env(self, prefix: &str) -> Self` | 以 `prefix` 開頭的環境變數；`__` 分隔巢狀 key，`APP_DATABASE__URL` 設定 `database.url` |
| `fn defaults<T: Serialize>(self, value: T) -> Self` | 從可序列化的值取得預設值，通常放在第一個 |
| `fn with(self, f: impl FnOnce(Figment) -> Figment) -> Self` | 任何其他 figment provider，例如 JSON 或 YAML |

環境變數名稱在移除前綴後轉成小寫，所以 `APP_PORT` 對應欄位 `port`。

---

## `ConfigModule`

```rust,ignore
pub struct ConfigModule { /* ... */ }
impl Module for ConfigModule { /* ... */ }
```

| 方法 | 說明 |
|---|---|
| `fn for_root(source: Source) -> ConfigModule` | 設定的根，從 `source` 讀取；在根模組註冊一次。每次呼叫都是獨立的模組，註冊兩個會被回報為 ambiguous，不會悄悄丟掉其中一個 |
| `fn for_feature() -> ConfigModule` | feature 模組自己的型別化設定，從 `for_root` 的 source 讀取；非 global，只有 import 它的模組看得到。沒有註冊 `for_root` 時，啟動錯誤會指出缺少 `RegisterConfigModuleForRootInTheRootModule` |
| `fn load<T: DeserializeOwned + Send + Sync + 'static>(self) -> Self` | 提供 `Arc<T>`，從整份設定反序列化 |
| `fn section<T: DeserializeOwned + Send + Sync + 'static>(self, key: impl Into<String>) -> Self` | 提供 `Arc<T>`，從 `key` 底下的區段反序列化；可以用 `"database.pool"` 這種點分路徑 |
| `fn local(self) -> Self` | 把 `for_root` 改成非 global：只有 import 它的模組看得到設定（此時 `for_feature` 找不到 root 的 source，兩者不要混用） |

- `for_root` 預設是 **global** 模組：任何模組都能注入載入的設定型別，不需要 import。
- 每個 `load` / `section` 都是一個 singleton provider，在啟動時反序列化。
- 缺少的 key、型別不符都是 bootstrap 錯誤，訊息包含型別、完整的 key 路徑與來源，例如 `` invalid configuration: invalid type: found string "many", expected u32 at `database.max_connections` (TOML source string) ``。
- 同一個型別只能被 `load` 或 `section` 一次，否則是 `Problem::DuplicateProvider`。

```rust
use hornero::prelude::*;
use hornero_config::{ConfigModule, Source};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
struct AppConfig {
    port: u16,
    name: String,
}

#[derive(Deserialize)]
struct PoolConfig {
    max_connections: u32,
}

#[derive(Injectable)]
struct Server {
    config: Arc<AppConfig>,
    pool: Arc<PoolConfig>,
}

#[module(
    imports = [
        ConfigModule::for_root(
            Source::new()
                .defaults(AppConfig { port: 3000, name: "hornero".into() })
                .toml_str("port = 8080\n[database.pool]\nmax_connections = 5"),
        )
        .load::<AppConfig>()
        .section::<PoolConfig>("database.pool"),
    ],
    providers = [Server],
)]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
let server = app.get::<Server>().unwrap();
assert_eq!(server.config.port, 8080);    // toml_str 覆寫了預設值
assert_eq!(server.config.name, "hornero");  // 沒被覆寫的維持預設值
assert_eq!(server.pool.max_connections, 5);
# }
```

需要在 config 之上建構其他 provider（例如 `SeaOrmModule::for_root_with`）時，直接注入 `Arc<AppConfig>`。

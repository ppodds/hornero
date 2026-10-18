# 10. 設定：hornero-config

Nest 的 `configService.get('DB_HOST')` 在 hornero 裡是**型別化的 serde struct**，以 `Arc<T>` 注入。設定在啟動時解析，缺少的 key 或型別錯誤會在 bind port 之前變成清楚的 bootstrap 錯誤，而不是在某個 request 裡才爆炸。

```toml
[dependencies]
hornero-config = { path = "../hornero/crates/hornero-config" }
serde = { version = "1", features = ["derive"] }
```

## 基本用法

```rust
use hornero::prelude::*;
use hornero_config::{ConfigModule, Source};
use serde::Deserialize;

#[derive(Deserialize)]
struct ServerConfig {
    address: String,
}

#[derive(Deserialize)]
struct DatabaseConfig {
    url: String,
    #[serde(default = "default_pool_size")]
    max_connections: u32,
}

fn default_pool_size() -> u32 {
    10
}

#[derive(Injectable)]
struct UserRepo {
    config: Arc<DatabaseConfig>, // 直接注入設定
}

#[module(
    imports = [
        ConfigModule::for_root(
            Source::new()
                .toml_str(r#"
                    [server]
                    address = "127.0.0.1:3000"

                    [database]
                    url = "sqlite::memory:"
                "#)
                .toml("config.toml") // 檔案存在就讀取，不存在就略過
                .env("SHOP_"),       // SHOP_DATABASE__URL 會覆寫 database.url
        )
        .section::<ServerConfig>("server")
        .section::<DatabaseConfig>("database"),
    ],
    providers = [UserRepo],
)]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = Hornero::new(AppModule).build().await.unwrap();
let database = app.get::<DatabaseConfig>().unwrap();
assert_eq!(database.url, "sqlite::memory:");
assert_eq!(database.max_connections, 10);
assert_eq!(app.get::<ServerConfig>().unwrap().address, "127.0.0.1:3000");
# }
```

## `Source`：設定從哪裡來

`Source` 依序合併多個來源，**後面的覆寫前面的**：

| 方法 | 來源 |
|---|---|
| `Source::new()` | 空的設定 |
| `.defaults(value)` | 任何 `Serialize` 值，通常放在最前面當預設值 |
| `.toml_str(text)` | 內嵌的 TOML，適合預設值與測試 |
| `.toml(path)` | TOML 檔案，從工作目錄往上層尋找；檔案不存在時略過 |
| `.toml_required(path)` | 同 `.toml`，但找不到檔案時啟動失敗，錯誤訊息指出檔名 |
| `.env(prefix)` | 以 `prefix` 開頭的環境變數 |
| `.with(\|figment\| ...)` | 任何其他 [figment](https://docs.rs/figment) provider（JSON、YAML……） |

環境變數的命名規則：去掉前綴後轉成小寫，`__`（兩個底線）表示巢狀。以 `.env("SHOP_")` 為例：

| 環境變數 | 設定 key |
|---|---|
| `SHOP_PORT=8080` | `port` |
| `SHOP_DATABASE__URL=postgres://...` | `database.url` |
| `SHOP_DATABASE__MAX_CONNECTIONS=20` | `database.max_connections` |

典型的正式環境設定：

```rust
use hornero_config::Source;
# #[derive(Default, serde::Serialize)]
# struct AppConfig {}

Source::new()
    .defaults(AppConfig::default())
    .toml("config.toml")
    .env("APP_")
# ;
```

## `ConfigModule`

| 方法 | 用途 |
|---|---|
| `ConfigModule::for_root(source)` | 設定的根，在根模組註冊一次 |
| `ConfigModule::for_feature()` | feature 模組自己的型別化設定（見下方） |
| `.load::<T>()` | 把整份設定解析成 `T`，提供 `Arc<T>` |
| `.section::<T>("key")` | 把某一段解析成 `T`；可以用點分隔的路徑，例如 `"database.pool"` |
| `.local()` | 把 `for_root` 改成非 global：只有 import 它的模組看得到（不能和 `for_feature` 並用） |

`ConfigModule::for_root` **預設是 global**：只要根模組 import 一次，所有模組都能注入設定型別。每個型別都會被 export。

### `for_feature`：feature 模組自己的設定

這和 NestJS 的 `ConfigModule.forRoot()` / `forFeature()` 一一對應。`for_root` 在根模組註冊一次並持有 `Source`；其他模組要嘛直接注入 root 載入的型別，要嘛用 `for_feature` 註冊自己的型別，從 root 的 source 讀取：

```rust
use hornero::prelude::*;
use hornero_config::{ConfigModule, Source};
# #[derive(serde::Deserialize)]
# struct MailConfig {}
# #[derive(Injectable)]
# struct Mailer {
#     config: Arc<MailConfig>,
# }

#[module(
    imports = [ConfigModule::for_feature().section::<MailConfig>("mail")],
    providers = [Mailer],
    exports = [Mailer],
)]
struct MailModule;

#[module(imports = [
    ConfigModule::for_root(Source::new().toml("config.toml").env("APP_")),
    MailModule,
])]
struct AppModule;
# Hornero::new(AppModule).validate().unwrap();
```

- `for_feature` 不是 global：`MailConfig` 只有 `MailModule` 看得到。
- 沒有註冊 `for_root` 時，啟動錯誤會說 `MailConfig` 需要 `RegisterConfigModuleForRootInTheRootModule`，意思就是請在根模組註冊 `ConfigModule::for_root(..)`。
- 每個 `for_root` 都是獨立的模組；同時註冊兩個，會在 `for_feature` 找不到唯一的 root 時回報 ambiguous。內容相同的 `for_feature` 會去重，不同的則是不同的模組。
- hornero 沒有字串 key 的 `ConfigService`，root 的 source 只透過 crate 內部的型別傳給 feature。

## 在其他 provider 中使用設定

設定是普通的 provider，factory 和其他模組都能依賴它。最常見的用法是把它交給整合模組的 `for_root_with`：

```rust
use hornero::prelude::*;
use hornero_config::{ConfigModule, Source};
use hornero_sea_orm::SeaOrmModule;
use hornero_sea_orm::sea_orm::ConnectOptions;
use serde::Deserialize;

#[derive(Deserialize)]
struct DatabaseConfig {
    url: String,
    max_connections: u32,
}

#[module(imports = [
    ConfigModule::for_root(Source::new().toml("config.toml").env("APP_"))
        .section::<DatabaseConfig>("database"),
    SeaOrmModule::for_root_with(|config: Arc<DatabaseConfig>| async move {
        let mut options = ConnectOptions::new(config.url.clone());
        options.max_connections(config.max_connections);
        Ok::<_, BoxError>(options)
    }),
])]
struct AppModule;

Hornero::new(AppModule).validate().unwrap(); // 依賴圖正確：SeaOrmModule 看得到 DatabaseConfig
```

## 錯誤

設定有問題時，錯誤訊息會指出哪個型別、哪個 key、來自哪個來源：

```rust
use hornero::prelude::*;
use hornero_config::{ConfigModule, Source};
use serde::Deserialize;

#[derive(Deserialize)]
struct DatabaseConfig {
    url: String,
    max_connections: u32,
}

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let source = Source::new().toml_str(r#"
    [database]
    url = "postgres://localhost/shop"
    max_connections = "many"
"#);
let config = ConfigModule::for_root(source).section::<DatabaseConfig>("database");

let err = Hornero::new(config).build().await.err().unwrap();
assert_eq!(
    err.to_string(),
    r#"failed to bootstrap application: 1 problem

[1] provider construction failed
    DatabaseConfig (in ConfigModule) failed to construct: invalid configuration: invalid type: found string "many", expected u32 at `database.max_connections` (TOML source string)
"#
);
# }
```

serde 能表達的規則（必填、型別、預設值、enum）直接寫在 struct 上。其他檢查（例如「port 不能是 0」）可以在 factory 或建構函式裡做，回傳錯誤就會變成 bootstrap 錯誤：

```rust
use hornero::prelude::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct RawServerConfig {
    port: u16,
}

/// 檢查過的設定：其他 provider 依賴這個型別
struct ServerConfig {
    port: u16,
}

#[injectable]
impl ServerConfig {
    fn new(raw: Arc<RawServerConfig>) -> Result<Self, BoxError> {
        if raw.port == 0 {
            return Err("server.port must not be 0".into());
        }
        Ok(Self { port: raw.port })
    }
}
```

## 測試

測試中不需要真的設定檔：

- 用 `override_value(DatabaseConfig { ... })` 直接替換設定型別（見 [測試](09-testing.md)）。
- 或在測試用的模組裡改用 `Source::new().toml_str(...)`。

`Source::toml` 找不到檔案時會略過，所以測試可以在任何工作目錄下執行。若應用程式沒有設定檔就無法運作，改用 `Source::toml_required`：找不到檔案時，啟動會直接失敗並指出檔名，而不是之後才出現 `missing field` 這類難以追查的錯誤。用 `toml` 時，若載入設定失敗而檔案又沒找到，錯誤訊息的結尾會註明「`config.toml` was not found」，提示可能是工作目錄不對。

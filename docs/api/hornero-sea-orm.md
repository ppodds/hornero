# `hornero-sea-orm` API 參考

[SeaORM](https://www.sea-ql.org/SeaORM/) 整合：在啟動時建立連線、以 `Arc<Db>` 注入、關閉時斷線。

```toml
[dependencies]
hornero-sea-orm = { path = "crates/hornero-sea-orm", features = ["sqlite"] }
# 或在自己的 sea-orm 依賴上啟用任何 driver feature，Cargo 會合併 feature
sea-orm = { version = "2", features = ["sqlx-postgres", "runtime-tokio-rustls", "macros"] }
```

| 項目 | 用途 |
|---|---|
| [`SeaOrmModule<Tag>`](#seaormmoduletag) | 提供並 export `Arc<Db<Tag>>` |
| [`Db<Tag>`](#dbtag) | 資料庫連線，可以 deref 成 `sea_orm::DatabaseConnection` |
| [`Primary`](#primary) | 預設連線的 tag |
| `sea_orm` | re-export 的 sea-orm crate |

| Feature | 效果 |
|---|---|
| `sqlite` | 啟用 sea-orm 的 `sqlx-sqlite` 與 `runtime-tokio-rustls` |

使用方式見 [資料庫](../guide/12-database.md)。

---

## `SeaOrmModule<Tag>`

```rust,ignore
pub struct SeaOrmModule<Tag = Primary> { /* ... */ }
impl<Tag: Send + Sync + 'static> Module for SeaOrmModule<Tag> { /* ... */ }
```

在根模組註冊一次 `for_root`（同 Nest 的 `TypeOrmModule.forRoot`），其他模組直接注入 `Arc<Db>`。每個 `SeaOrmModule` 值都是獨立的模組，同一個 tag 註冊兩次會得到 `Problem::AmbiguousDependency`，不會被當成同一個模組去重。

| 方法 | 說明 |
|---|---|
| `fn for_root(options: impl Into<ConnectOptions>) -> SeaOrmModule` | 以固定的設定連線；`&str` 與 `String` 的 URL 都可以 |
| `fn for_root_with<Args, F>(factory: F) -> SeaOrmModule` | 以注入的 provider 計算連線設定，相當於 Nest 的 `forRootAsync`；`F: FactoryFn<Args, Output = ConnectOptions>`，也就是 `\|cfg: Arc<AppConfig>\| async move { Ok::<_, BoxError>(ConnectOptions::new(&cfg.url)) }` |
| `fn tag<Other>(self) -> SeaOrmModule<Other>` | 改變 tag：`SeaOrmModule::for_root(url).tag::<Replica>()` 提供 `Db<Replica>` |
| `fn local(self) -> Self` | 改成非 global：只有 import 它的模組看得到連線 |

- 預設是 **global** 模組。
- 連線在啟動時建立，連不上資料庫是 bootstrap 錯誤（`Problem::Construction`）。
- 連線在 `on_application_shutdown` 關閉，晚於所有 `on_module_destroy`，所以其他 provider 在 `on_module_destroy` 裡仍可使用資料庫。
- 沒有 `forFeature([Entity])`：SeaORM 沒有每個 entity 一個 repository 的概念，feature 模組直接注入 `Arc<Db>`，再使用 `Entity::find()`。
- 每個 tag 只能有一個 `SeaOrmModule`。非 global 的連線可以用 `exports = [module(SeaOrmModule)]` 轉出。

---

## `Db<Tag>`

```rust,ignore
pub struct Db<Tag = Primary> { /* ... */ }
impl<Tag> Deref for Db<Tag> { type Target = DatabaseConnection; }
impl<Tag: 'static> Lifecycle for Db<Tag> { /* on_application_shutdown 關閉連線 */ }
```

| 方法 | 說明 |
|---|---|
| `fn new(conn: DatabaseConnection) -> Db<Tag>` | 包住現成的連線，例如在測試裡以 `override_provider` 換成另一個資料庫 |
| `fn conn(&self) -> &DatabaseConnection` | 底下的連線（也可以直接透過 `Deref` 使用） |

因為 `Db` 會 deref 成 `DatabaseConnection`，所以 `Entity::find().all(&**self.db)` 或 `self.db.conn()` 都可以當成 `ConnectionTrait` 使用；`self.db.begin().await?` 開始交易。

---

## `Primary`

```rust,ignore
pub struct Primary;
```

預設連線的 tag：`Db` 就是 `Db<Primary>`，`SeaOrmModule` 就是 `SeaOrmModule<Primary>`。

---

## 範例

```rust
use hornero::prelude::*;
use hornero_sea_orm::sea_orm::{ConnectOptions, ConnectionTrait, Statement};
use hornero_sea_orm::{Db, SeaOrmModule};

struct Replica;

#[derive(Injectable)]
struct Stats {
    primary: Arc<Db>,
    replica: Arc<Db<Replica>>,
}

impl Stats {
    async fn answer(&self, db: &impl ConnectionTrait) -> Result<i32, BoxError> {
        let statement = Statement::from_string(db.get_database_backend(), "SELECT 42 AS answer");
        let row = db.query_one_raw(statement).await?.ok_or("no row")?;
        Ok(row.try_get("", "answer")?)
    }
}

#[module(
    imports = [
        SeaOrmModule::for_root(ConnectOptions::new("sqlite::memory:").max_connections(1).to_owned()),
        SeaOrmModule::for_root("sqlite::memory:").tag::<Replica>(),
    ],
    providers = [Stats],
)]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() -> Result<(), BoxError> {
let app = Hornero::new(AppModule).build().await?;
let stats = app.get::<Stats>().unwrap();
assert_eq!(stats.answer(stats.primary.conn()).await?, 42);
assert_eq!(stats.answer(&**stats.replica).await?, 42);
app.close().await?; // 關閉兩個連線
# Ok(())
# }
```

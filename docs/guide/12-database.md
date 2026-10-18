# 12. 資料庫：hornero-sea-orm

`hornero-sea-orm` 提供 `SeaOrmModule`：在啟動時建立 [SeaORM](https://www.sea-ql.org/SeaORM/) 連線，以 `Arc<Db>` 提供給所有模組，並在關閉時關掉連線。

```toml
[dependencies]
hornero-sea-orm = { path = "../hornero/crates/hornero-sea-orm", features = ["sqlite"] }
sea-orm = { version = "2.0.4", default-features = false, features = ["macros", "runtime-tokio-rustls"] }
serde = { version = "1", features = ["derive"] }
```

- `hornero-sea-orm` 也 re-export 了 `sea_orm`，但 SeaORM 的 derive macro 會引用 `sea_orm::` 路徑，所以定義 entity 的 crate 仍需直接依賴 `sea-orm`。
- 資料庫 driver：`hornero-sea-orm` 目前只提供 `sqlite` feature。其他 driver 可以直接在你自己的 `sea-orm` 依賴上開啟（例如 `sqlx-postgres`），Cargo 會合併 features。撰寫本文時（2026-10），sea-orm 2.0 的 `sqlx-postgres` / `sqlx-mysql` 依賴了尚未發佈的 crate 版本，所以 hornero 暫時沒有提供這兩個 feature。

## 建立連線

| 寫法 | 用途 |
|---|---|
| `SeaOrmModule::for_root("sqlite::memory:")` | 固定的連線字串或 `ConnectOptions` |
| `SeaOrmModule::for_root_with(\|cfg: Arc<DbConfig>\| async move { ... })` | 由注入的 provider 計算連線設定，相當於 Nest 的 `forRootAsync` |
| `.tag::<Replica>()` | 提供 `Db<Replica>` 而不是 `Db`，見 [多個資料庫](#多個資料庫) |
| `.local()` | 改成非 global，只有 import 它的模組看得到連線 |

`SeaOrmModule` **預設是 global**：在根模組 import 一次（和 Nest 的 `TypeOrmModule.forRoot` 一樣），所有模組都能注入 `Arc<Db>`，其他模組不需要再 import 它。每次 `for_root` 都是獨立的實例：同一個 tag 註冊兩次，會讓 `Db` 變成 ambiguous dependency 的啟動錯誤，而不是悄悄丟掉其中一個。從設定讀取連線字串的寫法見 [設定](10-configuration.md#在其他-provider-中使用設定)。

## 完整範例

```rust
use hornero::prelude::*;
use hornero_sea_orm::{Db, SeaOrmModule};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ConnectOptions, ConnectionTrait, DbErr, EntityTrait, Schema,
    TransactionTrait,
};

mod note {
    use sea_orm::entity::prelude::*;
    use serde::Serialize;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
    #[sea_orm(table_name = "notes")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub text: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// 啟動時建立資料表。正式專案請在這裡執行 sea-orm-migration。
#[derive(Injectable)]
#[inject(hooks)]
struct NoteSchema {
    db: Arc<Db>,
}

impl Lifecycle for NoteSchema {
    async fn on_module_init(&self) -> Result<(), BoxError> {
        let schema = Schema::new(self.db.get_database_backend());
        self.db
            .execute(schema.create_table_from_entity(note::Entity).if_not_exists())
            .await?;
        Ok(())
    }
}

#[derive(Injectable)]
struct NoteRepo {
    db: Arc<Db>, // Db 會 deref 成 sea_orm::DatabaseConnection
}

impl NoteRepo {
    async fn all(&self) -> Result<Vec<note::Model>, DbErr> {
        note::Entity::find().all(self.db.conn()).await
    }

    async fn insert(&self, text: String) -> Result<note::Model, DbErr> {
        note::ActiveModel { id: ActiveValue::NotSet, text: ActiveValue::Set(text) }
            .insert(self.db.conn())
            .await
    }

    /// 全部成功或全部失敗
    async fn insert_all(&self, texts: Vec<String>) -> Result<(), DbErr> {
        let txn = self.db.begin().await?;
        for text in texts {
            note::ActiveModel { id: ActiveValue::NotSet, text: ActiveValue::Set(text) }
                .insert(&txn)
                .await?;
        }
        txn.commit().await
    }
}

#[controller("/notes")]
struct NoteController {
    notes: Arc<NoteRepo>,
}

#[routes]
impl NoteController {
    #[get("/")]
    async fn list(&self) -> Result<Json<Vec<note::Model>>, HttpError> {
        Ok(Json(self.notes.all().await?)) // DbErr 經由 ? 變成 500，細節只寫 log
    }

    #[post("/")]
    async fn create(&self, text: String) -> Result<(StatusCode, Json<note::Model>), HttpError> {
        Ok((StatusCode::CREATED, Json(self.notes.insert(text).await?)))
    }
}

#[module(controllers = [NoteController], providers = [NoteRepo, NoteSchema])]
struct NotesModule;

fn database() -> ConnectOptions {
    let mut options = ConnectOptions::new("sqlite::memory:");
    options.max_connections(1); // 每條 SQLite 記憶體連線都是獨立的資料庫
    options
}

#[module(imports = [SeaOrmModule::for_root(database()), NotesModule])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let client = app.client();

client.post("/notes").body("buy milk").await.assert_status(StatusCode::CREATED);
app.get::<NoteRepo>().insert_all(vec!["call mom".into(), "write docs".into()]).await.unwrap();

let notes: Vec<serde_json::Value> = client.get("/notes").await.json();
assert_eq!(notes.len(), 3);
assert_eq!(notes[0]["text"], "buy milk");

app.close().await.unwrap(); // 關閉連線
# }
```

使用 `Db` 的方式：

- `self.db.conn()` 回傳 `&DatabaseConnection`，可以傳給 SeaORM 的查詢方法（`.all(...)`、`.insert(...)`）。
- `Db` 會 deref 成 `DatabaseConnection`，所以 `self.db.begin()`、`self.db.execute(...)` 這類方法可以直接呼叫。

hornero 沒有 Nest 的 `TypeOrmModule.forFeature([Entity])`：SeaORM 沒有每個 entity 一個的 repository 物件，功能模組注入 `Arc<Db>`，直接使用 `Entity::find()` 即可。

## 生命週期

- **連線在啟動時建立**：資料庫連不上是 bootstrap 錯誤（`provider construction failed`），應用不會帶著壞掉的連線啟動。
- **連線在關閉時關掉**：`Db` 實作了 `Lifecycle`，會在 `on_application_shutdown` 關閉連線池。它是最早建構的 provider 之一，所以會在其他 provider 的 destroy hook 之後才關閉，那些 hook 仍然可以使用資料庫。
- **建表與 migration** 放在依賴 `Db` 的 provider 的 `on_module_init` 裡（如上例的 `NoteSchema`）。它會在所有 singleton 建構完成、開始接受 request 之前執行。

使用 sea-orm-migration 時：

<!-- ignore: 需要 sea-orm-migration，docs 沒有依賴它，無法編譯 -->
```rust,ignore
impl Lifecycle for Migrations {
    async fn on_module_init(&self) -> Result<(), BoxError> {
        Migrator::up(self.db.conn(), None).await?;
        Ok(())
    }
}
```

## Repository 與可測試性

把資料存取放在 trait 後面，測試時就能替換成 in-memory 實作，不需要資料庫：

<!-- ignore: 節錄自 examples/todo/src/todo/repo.rs，`/* ... */` 省略了實作 -->
```rust,ignore
pub trait TodoRepo: Send + Sync {
    fn all(&self) -> BoxFuture<'_, Result<Vec<Todo>, DbErr>>;
}

#[derive(Injectable)]
pub struct SeaOrmTodoRepo {
    db: Arc<Db>,
}

impl TodoRepo for SeaOrmTodoRepo { /* ... */ }

#[module(providers = [bind!(dyn TodoRepo => SeaOrmTodoRepo), TodoService])]
pub struct TodoModule;
```

完整寫法見 `examples/todo/src/todo/repo.rs`。只在資料庫這種邊界加 trait，service 保持具體型別即可。

## 測試

兩種做法：

- **替換 repository**：`TestApp::new(AppModule).override_provider::<dyn TodoRepo>(Arc::new(InMemoryRepo::default()))`。被替換掉的 `SeaOrmTodoRepo` 不會被建構；如果應用裡沒有其他東西依賴 `Db`，連線也不會建立。
- **SQLite 記憶體資料庫**：用 `sqlite::memory:`，並設定 `max_connections(1)`。每條 SQLite 記憶體連線都是一個獨立的資料庫，連線池裡有多條連線時，A 連線建的表在 B 連線上看不到。

正式環境用 Postgres、測試用 SQLite 時，要注意兩者在 SQL 方言與型別上的差異。需要完全一致的行為時，請在測試中連到真正的 Postgres（例如用 testcontainers）。

## 多個資料庫

同一型別就是同一個 token，所以第二條連線要用不同的 tag：

```rust
use hornero::prelude::*;
use hornero_sea_orm::{Db, SeaOrmModule};

/// 唯讀副本的 tag
struct Replica;

#[derive(Injectable)]
struct ReportService {
    primary: Arc<Db>,          // Db<Primary>
    replica: Arc<Db<Replica>>, // 另一條連線
}

#[module(
    imports = [
        SeaOrmModule::for_root("sqlite::memory:"),
        SeaOrmModule::for_root("sqlite::memory:").tag::<Replica>(),
    ],
    providers = [ReportService],
)]
struct AppModule;

Hornero::new(AppModule).validate().unwrap();
```

`Db` 是 `Db<Primary>` 的簡寫。每個 tag 是不同的模組型別，所以兩個 `SeaOrmModule` 不會被去重。

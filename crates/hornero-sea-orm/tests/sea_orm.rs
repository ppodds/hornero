use hornero::prelude::*;
use hornero_sea_orm::sea_orm::{ConnectOptions, ConnectionTrait, DbBackend, Statement};
use hornero_sea_orm::{Db, SeaOrmModule};

struct Replica;

struct DatabaseUrl(String);

#[derive(Injectable)]
struct Probe {
    db: Arc<Db>,
}

impl Probe {
    async fn answer(&self) -> i64 {
        let row = self
            .db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT 40 + 2 AS answer",
            ))
            .await
            .unwrap()
            .unwrap();
        row.try_get("", "answer").unwrap()
    }
}

#[module(providers = [Probe], exports = [Probe])]
struct ProbeModule;

fn memory() -> ConnectOptions {
    let mut options = ConnectOptions::new("sqlite::memory:");
    options.max_connections(1).sqlx_logging(false);
    options
}

#[tokio::test]
async fn provides_a_global_connection() {
    #[module(imports = [SeaOrmModule::for_root(memory()), ProbeModule])]
    struct App;

    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<Probe>().unwrap().answer().await, 42);
}

#[tokio::test]
async fn a_local_connection_can_be_re_exported() {
    #[module(
        imports = [SeaOrmModule::for_root(memory()).local()],
        exports = [module(SeaOrmModule)],
    )]
    struct DatabaseModule;

    #[module(imports = [DatabaseModule], providers = [Probe], exports = [Probe])]
    struct App;

    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<Probe>().unwrap().answer().await, 42);
}

#[tokio::test]
async fn options_can_come_from_injected_providers() {
    #[module(global, providers = [value(DatabaseUrl("sqlite::memory:".into()))], exports = [DatabaseUrl])]
    struct Settings;

    #[module(imports = [
        Settings,
        SeaOrmModule::for_root_with(|url: Arc<DatabaseUrl>| async move {
            let mut options = ConnectOptions::new(url.0.clone());
            options.sqlx_logging(false);
            Ok::<_, BoxError>(options)
        }),
        ProbeModule,
    ])]
    struct App;

    let app = Hornero::new(App).build().await.unwrap();
    assert_eq!(app.get::<Probe>().unwrap().answer().await, 42);
}

#[tokio::test]
async fn tagged_connections_coexist() {
    #[module(imports = [
        SeaOrmModule::for_root(memory()),
        SeaOrmModule::for_root(memory()).tag::<Replica>(),
    ])]
    struct App;

    let app = Hornero::new(App).build().await.unwrap();
    let primary = app.get::<Db>().unwrap();
    let replica = app.get::<Db<Replica>>().unwrap();
    primary
        .execute_unprepared("CREATE TABLE only_on_primary (id INTEGER)")
        .await
        .unwrap();
    assert!(
        replica
            .execute_unprepared("SELECT * FROM only_on_primary")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn connection_failures_abort_the_bootstrap() {
    #[module(imports = [SeaOrmModule::for_root("sqlite:///definitely/missing/dir/db.sqlite?mode=ro")])]
    struct App;

    let err = Hornero::new(App).build().await.err().unwrap().to_string();
    assert!(
        err.contains("Db (in SeaOrmModule) failed to construct"),
        "{err}"
    );
}

#[tokio::test]
async fn the_connection_is_closed_on_shutdown() {
    #[module(imports = [SeaOrmModule::for_root(memory())])]
    struct App;

    let app = Hornero::new(App).build().await.unwrap();
    let db = app.get::<Db>().unwrap();
    db.execute_unprepared("SELECT 1").await.unwrap();
    app.close().await.unwrap();
    assert!(
        db.execute_unprepared("SELECT 1").await.is_err(),
        "pool is closed"
    );
}

#[test]
fn two_roots_for_one_tag_are_ambiguous() {
    #[module(imports = [
        SeaOrmModule::for_root(memory()),
        SeaOrmModule::for_root(memory()),
        ProbeModule,
    ])]
    struct App;

    let err = Hornero::new(App).validate().unwrap_err();
    insta::assert_snapshot!(err.to_string());
}

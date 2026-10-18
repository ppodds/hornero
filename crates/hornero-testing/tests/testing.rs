use std::sync::Mutex;

use hornero::prelude::*;
use hornero_testing::TestApp;
use http::StatusCode;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
struct Item {
    name: String,
}

trait Inventory: Send + Sync {
    fn list(&self) -> Vec<Item>;
    fn add(&self, item: Item);
}

#[derive(Injectable)]
struct RealInventory {
    #[inject(default)]
    items: Mutex<Vec<Item>>,
}

impl Inventory for RealInventory {
    fn list(&self) -> Vec<Item> {
        self.items.lock().unwrap().clone()
    }
    fn add(&self, item: Item) {
        self.items.lock().unwrap().push(item);
    }
}

#[derive(Default)]
struct FakeInventory(Mutex<Vec<Item>>);

impl Inventory for FakeInventory {
    fn list(&self) -> Vec<Item> {
        let mut items = self.0.lock().unwrap().clone();
        items.push(Item {
            name: "fake".into(),
        });
        items
    }
    fn add(&self, item: Item) {
        self.0.lock().unwrap().push(item);
    }
}

#[derive(Injectable)]
struct TokenGuard;

impl Guard for TokenGuard {
    async fn can_activate(&self, ctx: &mut GuardContext<'_>) -> Result<(), HttpError> {
        match ctx.parts.headers.get("authorization") {
            Some(value) if value == "Bearer secret" => Ok(()),
            _ => Err(HttpError::unauthorized("bad token")),
        }
    }
}

struct Open;

impl Guard for Open {
    async fn can_activate(&self, _: &mut GuardContext<'_>) -> Result<(), HttpError> {
        Ok(())
    }
}

#[controller("/items")]
#[guards(TokenGuard)]
struct ItemController {
    inventory: Arc<dyn Inventory>,
}

#[routes]
impl ItemController {
    #[get("/")]
    async fn list(&self) -> Json<Vec<Item>> {
        Json(self.inventory.list())
    }

    #[post("/")]
    async fn add(&self, Json(item): Json<Item>) -> StatusCode {
        self.inventory.add(item);
        StatusCode::CREATED
    }
}

#[module(controllers = [ItemController], providers = [bind!(dyn Inventory => RealInventory)])]
struct AppModule;

#[tokio::test]
async fn client_sends_requests_through_the_full_pipeline() {
    let app = TestApp::new(AppModule).build().await.unwrap();
    let client = app.client();

    client
        .get("/items")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    let authed = client.clone().with_bearer("secret");
    authed
        .post("/items")
        .json(&Item {
            name: "lamp".into(),
        })
        .await
        .assert_status(StatusCode::CREATED);
    let items: Vec<Item> = authed
        .get("/items")
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        items,
        [Item {
            name: "lamp".into()
        }]
    );

    let response = client.get("/items").bearer("wrong").send().await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.header("content-type"),
        Some("application/problem+json")
    );
    assert_eq!(response.json::<serde_json::Value>()["detail"], "bad token");
}

#[tokio::test]
async fn providers_and_guards_can_be_overridden() {
    let fake: Arc<dyn Inventory> = Arc::new(FakeInventory::default());
    let app = TestApp::new(AppModule)
        .override_provider(fake)
        .override_guard::<TokenGuard>(Open)
        .build()
        .await
        .unwrap();
    let items: Vec<Item> = app.client().get("/items").await.json();
    assert_eq!(
        items,
        [Item {
            name: "fake".into()
        }]
    );
    assert_eq!(app.get::<dyn Inventory>().list().len(), 1);
}

#[tokio::test]
#[should_panic(expected = "no singleton")]
async fn get_panics_for_unknown_types() {
    let app = TestApp::new(AppModule).build().await.unwrap();
    app.get::<String>();
}

#[tokio::test]
async fn configure_reaches_the_hornero_builder() {
    let app = TestApp::new(AppModule)
        .override_guard::<TokenGuard>(Open)
        .configure(|hornero| hornero.global_prefix("/api"))
        .build()
        .await
        .unwrap();
    app.client()
        .get("/api/items")
        .await
        .assert_status(StatusCode::OK);
    app.close().await.unwrap();
}

#[tokio::test]
async fn requests_carry_a_loopback_peer_address_that_can_be_changed() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    struct PeerModule;
    impl Module for PeerModule {
        fn define(&self, m: &mut ModuleDef) {
            m.mount_router(
                "/",
                axum::Router::new().route(
                    "/peer",
                    axum::routing::get(|ConnectInfo(addr): ConnectInfo<SocketAddr>| async move {
                        addr.to_string()
                    }),
                ),
            );
        }
    }
    let app = TestApp::new(PeerModule).build().await.unwrap();
    let client = app.client();
    assert_eq!(client.get("/peer").await.text(), "127.0.0.1:40000");
    let peer: SocketAddr = "10.1.2.3:5555".parse().unwrap();
    assert_eq!(
        client.get("/peer").peer_addr(peer).await.text(),
        "10.1.2.3:5555"
    );
}

//! The quickstart application from `docs/guide/01-getting-started.md`.
//!
//! ```sh
//! cargo run -p hornero-docs --example hello
//! curl localhost:3000/hello/hornero
//! ```

// ANCHOR: app
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
// ANCHOR_END: app

// ANCHOR: main
#[tokio::main]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::fmt::init();
    Hornero::new(AppModule).listen("127.0.0.1:3000").await
}
// ANCHOR_END: main

// ANCHOR: tests
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
// ANCHOR_END: tests

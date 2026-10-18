use hornero::prelude::*;
use hornero_testing::TestApp;
use hornero_validation::Valid;
use hornero_validation::garde::Validate;
use http_status::StatusCode;
use serde::Deserialize;
use serde_json::json;

mod http_status {
    pub use hornero::axum::http::StatusCode;
}

#[derive(Deserialize, Validate)]
struct CreateUser {
    #[garde(length(min = 2, max = 20))]
    name: String,
    #[garde(range(min = 18))]
    age: u8,
}

#[derive(Deserialize, Validate)]
struct Paging {
    #[garde(range(min = 1, max = 100))]
    limit: u32,
}

#[controller("/users")]
struct UserController;

#[routes]
impl UserController {
    #[post("/")]
    async fn create(&self, Valid(Json(input)): Valid<Json<CreateUser>>) -> String {
        format!("{} ({})", input.name, input.age)
    }

    #[get("/")]
    async fn list(&self, Valid(Query(paging)): Valid<Query<Paging>>) -> String {
        format!("limit={}", paging.limit)
    }
}

#[module(controllers = [UserController])]
struct AppModule;

#[tokio::test]
async fn valid_payloads_pass_through() {
    let client = TestApp::new(AppModule).build().await.unwrap().client();
    let response = client
        .post("/users")
        .json(&json!({ "name": "ada", "age": 36 }))
        .await
        .assert_status(StatusCode::OK);
    assert_eq!(response.text(), "ada (36)");
    assert_eq!(client.get("/users?limit=10").await.text(), "limit=10");
}

#[tokio::test]
async fn rule_violations_are_422_with_every_field_error() {
    let client = TestApp::new(AppModule).build().await.unwrap().client();
    let response = client
        .post("/users")
        .json(&json!({ "name": "a", "age": 12 }))
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    let problem: serde_json::Value = response.json();
    assert_eq!(problem["detail"], "validation failed");
    let paths: Vec<&str> = problem["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| error["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["age", "name"]);

    let response = client
        .get("/users?limit=500")
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        response.json::<serde_json::Value>()["errors"][0]["path"],
        "limit"
    );
}

#[tokio::test]
async fn malformed_input_becomes_a_problem_document() {
    let client = TestApp::new(AppModule).build().await.unwrap().client();

    let response = client
        .post("/users")
        .header("content-type", "application/json")
        .body("{not json")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        response.header("content-type"),
        Some("application/problem+json")
    );

    let response = client
        .get("/users")
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    let detail = response.json::<serde_json::Value>()["detail"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(detail.contains("missing field `limit`"), "{detail}");
}

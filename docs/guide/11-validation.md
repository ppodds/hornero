# 11. 驗證：hornero-validation

Rust 的 extractor 已經會把 request 解析成型別：欄位缺少或型別不對，`Json<T>` 就會拒絕。`hornero-validation` 再補上 serde 表達不了的規則（長度、範圍、格式），相當於 Nest 的 `ValidationPipe`。

```toml
[dependencies]
hornero-validation = { path = "../hornero/crates/hornero-validation" }
garde = { version = "0.23", features = ["derive", "full"] }
serde = { version = "1", features = ["derive"] }
```

`garde` 必須加在你自己的依賴裡：它的 derive macro 產生的程式碼會引用 `::garde`。

## `Valid<E>`

把 extractor 包進 `Valid`，解析成功後會再執行 [garde](https://docs.rs/garde) 驗證：

```rust
use garde::Validate;
use hornero::prelude::*;
use hornero_validation::Valid;
use serde::Deserialize;

#[derive(Deserialize, Validate)]
struct Address {
    #[garde(length(min = 1))]
    city: String,
}

fn not_reserved(name: &str, _context: &()) -> garde::Result {
    if name.eq_ignore_ascii_case("admin") {
        return Err(garde::Error::new("this name is reserved"));
    }
    Ok(())
}

#[derive(Deserialize, Validate)]
struct CreateUser {
    #[garde(length(min = 3, max = 20), custom(not_reserved))]
    name: String,
    #[garde(email)]
    email: String,
    #[garde(range(min = 13))]
    age: u8,
    #[garde(dive)] // 驗證巢狀的 struct
    address: Address,
    #[garde(skip)] // 每個欄位都要有規則，或明確標成 skip
    nickname: Option<String>,
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
    fn create(&self, Valid(Json(user)): Valid<Json<CreateUser>>) -> (StatusCode, String) {
        (StatusCode::CREATED, user.name)
    }

    #[get("/")]
    fn list(&self, Valid(Query(paging)): Valid<Query<Paging>>) -> String {
        format!("limit={}", paging.limit)
    }
}

#[module(controllers = [UserController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = hornero_testing::TestApp::new(AppModule).build().await.unwrap();
let client = app.client();

let invalid = serde_json::json!({
    "name": "ad",
    "email": "nope",
    "age": 7,
    "address": { "city": "" },
});
let res = client.post("/users").json(&invalid).await;
assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
assert_eq!(
    res.json::<serde_json::Value>(),
    serde_json::json!({
        "type": "about:blank",
        "title": "Unprocessable Entity",
        "status": 422,
        "detail": "validation failed",
        "errors": [
            { "path": "address.city", "message": "length is lower than 1" },
            { "path": "age", "message": "lower than 13" },
            { "path": "email", "message": "not a valid email: value is missing `@`" },
            { "path": "name", "message": "length is lower than 3" },
        ]
    })
);

assert_eq!(client.get("/users?limit=5").await.text(), "limit=5");
assert_eq!(client.get("/users?limit=0").await.status(), StatusCode::UNPROCESSABLE_ENTITY);
# }
```

`Valid` 可以包住以下 extractor，內層型別需要實作 `garde::Validate`，且 context 型別要實作 `Default`（沒有自訂 context 時就是 `()`）：

| Extractor | 驗證 |
|---|---|
| `Valid<Json<T>>` | JSON body |
| `Valid<Query<T>>` | query string |
| `Valid<Form<T>>` | form body |
| `Valid<Path<T>>` | 路徑參數 |

要包住其他 extractor，替它實作 `hornero_validation::Validated` trait 即可：

```rust
use garde::Validate;
use hornero_validation::Validated;
# struct MyExtractor<T>(T);

impl<T: Validate> Validated for MyExtractor<T> {
    type Target = T;
    fn target(&self) -> &T { &self.0 }
}
```

## 錯誤格式

所有輸入錯誤都會變成 problem+json 的 `HttpError`，因此也會經過 exception filter：

| 情況 | 狀態碼 | `detail` | `errors` |
|---|---|---|---|
| 驗證規則不通過 | 422 | `validation failed` | 每個欄位錯誤一筆 `{ path, message }`，依 path 排序 |
| JSON 語法錯誤 | 400 | axum 的訊息，例如 `Failed to parse the request body as JSON: ...` | 無 |
| 缺少欄位或型別不對 | 422 | `Failed to deserialize the JSON body into the target type: ...` | 無 |
| 缺少 `Content-Type: application/json` | 415 | `Expected request with ...` | 無 |
| query string 無法解析 | 400 | `Failed to deserialize query string: ...` | 無 |

要分辨「格式錯誤」與「規則不通過」，看回應裡有沒有 `errors` 欄位即可。

不用 `Valid` 的 axum extractor（例如單純的 `Json<T>`）被拒絕時，回應是 axum 的純文字，不會經過 filter。要讓所有輸入錯誤都長得一樣，請一律使用 `Valid`，或 `Rejection = HttpError` 的自訂 extractor。

## 常用的 garde 規則

| 規則 | 範例 |
|---|---|
| 長度 | `#[garde(length(min = 1, max = 200))]` |
| 數值範圍 | `#[garde(range(min = 1, max = 100))]` |
| 格式 | `#[garde(email)]`、`#[garde(url)]`、`#[garde(ascii)]`、`#[garde(pattern(r"^[a-z]+$"))]` |
| `Option<T>` | 規則套用在 `Some` 的值上，`None` 視為通過；搭配 `#[garde(required)]` 可以要求必填 |
| 巢狀 struct 與集合 | `#[garde(dive)]` |
| 自訂規則 | `#[garde(custom(my_check))]`，簽名是 `fn(&T, &Context) -> garde::Result` |
| 不驗證 | `#[garde(skip)]` |

完整清單見 [garde 文件](https://docs.rs/garde)。

## 驗證放在哪裡

- **輸入格式**（長度、範圍、格式）用 `Valid`，放在 DTO 上。
- **需要查資料的規則**（email 是否已註冊、庫存是否足夠）放在 service，回傳 `HttpError::conflict` 或 `unprocessable`。
- **權限**放在 guard，不要混進 DTO 驗證。

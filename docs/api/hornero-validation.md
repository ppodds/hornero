# `hornero-validation` API 參考

以 [garde](https://docs.rs/garde) 驗證 request，相當於 Nest 的 `ValidationPipe`。serde 已經把 request 解析成型別，`Valid<E>` 再加上 serde 表達不了的規則（長度、範圍、格式…）。

```toml
[dependencies]
hornero-validation = { path = "crates/hornero-validation" }
garde = { version = "0.23", features = ["derive"] }   # derive macro 產生的程式碼使用 ::garde 路徑
```

| 項目 | 用途 |
|---|---|
| [`Valid<E>`](#valide) | 包住 extractor，驗證它取出的值 |
| [`Validated`](#trait-validated) | 可以被 `Valid` 包住的 extractor |
| `garde` | re-export 的 garde crate |

使用方式見 [驗證](../guide/11-validation.md)。

---

## `Valid<E>`

```rust,ignore
#[derive(Debug, Clone, Copy, Default)]
pub struct Valid<E>(pub E);

impl<S, E> FromRequest<S> for Valid<E>
where E: FromRequest<S> + Validated, <E::Target as Validate>::Context: Default { /* ... */ }

impl<S, E> FromRequestParts<S> for Valid<E>
where E: FromRequestParts<S> + Validated, <E::Target as Validate>::Context: Default { /* ... */ }
```

| 寫法 | 驗證的對象 |
|---|---|
| `Valid(Json(body)): Valid<Json<T>>` | JSON body |
| `Valid(Form(form)): Valid<Form<T>>` | form body |
| `Valid(Query(q)): Valid<Query<T>>` | query string |
| `Valid(Path(p)): Valid<Path<T>>` | 路徑參數（`T` 需要是 struct，例如 `#[derive(Deserialize, Validate)] struct Params { id: u64 }`） |

`T` 必須實作 `garde::Validate`，且它的 `Context` 實作 `Default`（沒有自訂 context 時就是 `()`）。

### 錯誤

| 狀況 | 回應 |
|---|---|
| 驗證失敗 | `422 Unprocessable Entity`，`detail` 為 `validation failed`，`errors` 列出每個欄位錯誤 |
| 內層 extractor 拒絕（JSON 格式錯誤、缺少 query 參數…） | 原本的狀態碼（400、415、422…），`detail` 為 axum 的錯誤訊息 |

兩者都會渲染成 problem+json，也都帶有 `HttpError`，所以 exception filter 可以重新渲染。`errors` 的每一筆是 `{ "path": "...", "message": "..." }`，巢狀欄位的 path 像 `address.city` 或 `tags[0]`。

---

## `trait Validated`

```rust,ignore
pub trait Validated {
    type Target: Validate;
    fn target(&self) -> &Self::Target;
}
```

| 項目 | 說明 |
|---|---|
| `Target` | 要驗證的型別 |
| `target(&self)` | 取出要驗證的值 |

已經為 `Json<T>`、`Form<T>`、`Query<T>`、`Path<T>` 實作。為自己的 extractor 實作後，就能寫 `Valid<MyExtractor<T>>`。

---

## 範例

```rust
use garde::Validate;
use hornero::prelude::*;
use hornero_testing::TestApp;
use hornero_validation::Valid;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize, Validate)]
struct Search {
    #[garde(length(min = 2))]
    q: String,
    #[garde(range(max = 50))]
    limit: u32,
}

#[controller("/search")]
struct SearchController;

#[routes]
impl SearchController {
    #[get("/")]
    fn search(&self, Valid(Query(search)): Valid<Query<Search>>) -> String {
        format!("{} (max {})", search.q, search.limit)
    }
}

#[module(controllers = [SearchController])]
struct AppModule;

# #[tokio::main(flavor = "current_thread")]
# async fn main() {
let app = TestApp::new(AppModule).build().await.unwrap();

let res = app.client().get("/search?q=rust&limit=10").await;
assert_eq!(res.text(), "rust (max 10)");

let res = app.client().get("/search?q=r&limit=99").await;
assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
let problem: Value = res.json();
assert_eq!(problem["detail"], "validation failed");
assert_eq!(problem["errors"].as_array().unwrap().len(), 2);

let res = app.client().get("/search?q=rust").await; // 缺少 limit
assert_eq!(res.status(), StatusCode::BAD_REQUEST);
assert_eq!(res.header("content-type"), Some("application/problem+json"));
# }
```

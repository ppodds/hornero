use garde::Validate;
use hornero::openapi::ToSchema;
use serde::Deserialize;

/// A new todo.
#[derive(Deserialize, Validate, ToSchema)]
pub struct CreateTodo {
    /// 1 to 200 characters.
    #[garde(length(chars, min = 1, max = 200))]
    #[schema(min_length = 1, max_length = 200)]
    pub title: String,
}

/// Changes to a todo; absent fields stay as they are.
#[derive(Deserialize, Validate, ToSchema)]
pub struct UpdateTodo {
    /// 1 to 200 characters.
    #[garde(inner(length(chars, min = 1, max = 200)))]
    #[schema(min_length = 1, max_length = 200)]
    pub title: Option<String>,
    #[garde(skip)]
    pub done: Option<bool>,
}

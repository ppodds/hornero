use hornero::prelude::*;
use hornero_validation::Valid;

use super::dto::{CreateTodo, UpdateTodo};
use super::entity::Todo;
use super::error::TodoErrorFilter;
use super::service::TodoService;
use crate::auth::{AuthGuard, AuthUser, Roles, RolesGuard};

/// Handlers raise `TodoError` with `?`; `TodoErrorFilter` maps it, other errors reach the global
/// `ProblemFilter`.
///
/// The OpenAPI document (`/docs`) is inferred from these signatures: `Path`, `Json` and `Valid`
/// arguments, `Json` and `Result` return types, and doc comments. `#[api_response]` adds what the
/// signatures cannot say, such as a 201 or the 404 that `TodoErrorFilter` produces.
#[controller("/todos")]
#[guards(AuthGuard)]
#[filters(TodoErrorFilter)]
#[api_response(401, description = "Missing or unknown bearer token")]
pub struct TodoController {
    todos: Arc<TodoService>,
}

#[routes]
impl TodoController {
    /// Lists every todo.
    #[get("/")]
    async fn list(&self) -> Result<Json<Vec<Todo>>, HttpError> {
        Ok(Json(self.todos.list().await?))
    }

    /// Finds one todo.
    #[get("/{id}")]
    #[api_response(404, description = "No todo has this id")]
    async fn find_one(&self, Path(id): Path<i32>) -> Result<Json<Todo>, HttpError> {
        Ok(Json(self.todos.get(id).await?))
    }

    /// Creates a todo owned by the caller.
    #[post("/")]
    #[api_response(201, description = "The created todo")]
    async fn create(
        &self,
        user: AuthUser,
        Valid(Json(input)): Valid<Json<CreateTodo>>,
    ) -> Result<(StatusCode, Json<Todo>), HttpError> {
        let todo = self.todos.create(input, &user.name).await?;
        Ok((StatusCode::CREATED, Json(todo)))
    }

    /// Renames a todo or marks it done.
    #[patch("/{id}")]
    #[api_response(404, description = "No todo has this id")]
    async fn update(
        &self,
        Path(id): Path<i32>,
        Valid(Json(input)): Valid<Json<UpdateTodo>>,
    ) -> Result<Json<Todo>, HttpError> {
        Ok(Json(self.todos.update(id, input).await?))
    }

    /// Deletes a todo. Admins only.
    #[delete("/{id}")]
    #[guards(RolesGuard)]
    #[meta(Roles(&["admin"]))]
    #[api_response(204)]
    #[api_response(403, description = "The caller is not an admin")]
    #[api_response(404, description = "No todo has this id")]
    async fn remove(&self, Path(id): Path<i32>) -> Result<StatusCode, HttpError> {
        self.todos.delete(id).await?;
        Ok(StatusCode::NO_CONTENT)
    }
}

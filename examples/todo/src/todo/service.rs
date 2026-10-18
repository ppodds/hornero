use hornero::prelude::*;

use super::audit::AuditLog;
use super::dto::{CreateTodo, UpdateTodo};
use super::entity::Todo;
use super::error::TodoError;
use super::repo::{TodoPatch, TodoRepo};

/// Business logic. A concrete type: only the repository sits behind a trait.
///
/// It returns `TodoError` and knows nothing about HTTP; handlers raise it with `?` and
/// `TodoErrorFilter` picks the status.
#[derive(Injectable)]
pub struct TodoService {
    repo: Arc<dyn TodoRepo>,
    audit: Arc<AuditLog>,
}

impl TodoService {
    pub async fn list(&self) -> Result<Vec<Todo>, TodoError> {
        Ok(self.repo.all().await?)
    }

    pub async fn get(&self, id: i32) -> Result<Todo, TodoError> {
        self.repo.find(id).await?.ok_or(TodoError::NotFound(id))
    }

    pub async fn create(&self, input: CreateTodo, owner: &str) -> Result<Todo, TodoError> {
        let todo = self.repo.insert(input.title, owner.to_owned()).await?;
        self.audit.record("create", todo.id);
        Ok(todo)
    }

    pub async fn update(&self, id: i32, input: UpdateTodo) -> Result<Todo, TodoError> {
        let patch = TodoPatch {
            title: input.title,
            done: input.done,
        };
        let todo = self
            .repo
            .update(id, patch)
            .await?
            .ok_or(TodoError::NotFound(id))?;
        self.audit.record("update", id);
        Ok(todo)
    }

    pub async fn delete(&self, id: i32) -> Result<(), TodoError> {
        if !self.repo.delete(id).await? {
            return Err(TodoError::NotFound(id));
        }
        self.audit.record("delete", id);
        Ok(())
    }
}

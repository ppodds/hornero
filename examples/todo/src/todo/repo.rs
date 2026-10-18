//! Persistence behind a trait: the one boundary worth abstracting, so tests can swap it out.
//! Services stay concrete types.

use hornero::BoxFuture;
use hornero::prelude::*;
use hornero_sea_orm::Db;
use sea_orm::{ActiveModelTrait, ActiveValue, DbErr, EntityTrait, QueryOrder};

use super::entity::{self, Todo};

pub struct TodoPatch {
    pub title: Option<String>,
    pub done: Option<bool>,
}

/// `async fn` in traits is not dyn-compatible, so the methods return boxed futures.
pub trait TodoRepo: Send + Sync {
    fn all(&self) -> BoxFuture<'_, Result<Vec<Todo>, DbErr>>;
    fn find(&self, id: i32) -> BoxFuture<'_, Result<Option<Todo>, DbErr>>;
    fn insert(&self, title: String, created_by: String) -> BoxFuture<'_, Result<Todo, DbErr>>;
    fn update(&self, id: i32, patch: TodoPatch) -> BoxFuture<'_, Result<Option<Todo>, DbErr>>;
    fn delete(&self, id: i32) -> BoxFuture<'_, Result<bool, DbErr>>;
}

#[derive(Injectable)]
pub struct SeaOrmTodoRepo {
    db: Arc<Db>,
}

impl TodoRepo for SeaOrmTodoRepo {
    fn all(&self) -> BoxFuture<'_, Result<Vec<Todo>, DbErr>> {
        Box::pin(async move {
            entity::Entity::find()
                .order_by_asc(entity::Column::Id)
                .all(self.db.conn())
                .await
        })
    }

    fn find(&self, id: i32) -> BoxFuture<'_, Result<Option<Todo>, DbErr>> {
        Box::pin(async move { entity::Entity::find_by_id(id).one(self.db.conn()).await })
    }

    fn insert(&self, title: String, created_by: String) -> BoxFuture<'_, Result<Todo, DbErr>> {
        Box::pin(async move {
            entity::ActiveModel {
                id: ActiveValue::NotSet,
                title: ActiveValue::Set(title),
                done: ActiveValue::Set(false),
                created_by: ActiveValue::Set(created_by),
            }
            .insert(self.db.conn())
            .await
        })
    }

    fn update(&self, id: i32, patch: TodoPatch) -> BoxFuture<'_, Result<Option<Todo>, DbErr>> {
        Box::pin(async move {
            let Some(existing) = entity::Entity::find_by_id(id).one(self.db.conn()).await? else {
                return Ok(None);
            };
            let mut model: entity::ActiveModel = existing.into();
            if let Some(title) = patch.title {
                model.title = ActiveValue::Set(title);
            }
            if let Some(done) = patch.done {
                model.done = ActiveValue::Set(done);
            }
            model.update(self.db.conn()).await.map(Some)
        })
    }

    fn delete(&self, id: i32) -> BoxFuture<'_, Result<bool, DbErr>> {
        Box::pin(async move {
            let result = entity::Entity::delete_by_id(id)
                .exec(self.db.conn())
                .await?;
            Ok(result.rows_affected > 0)
        })
    }
}

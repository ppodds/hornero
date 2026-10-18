use hornero::openapi::ToSchema;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// A todo. In the OpenAPI document it is `Todo`, not sea-orm's `Model`.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize, ToSchema)]
#[sea_orm(table_name = "todos")]
#[schema(as = Todo)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub title: String,
    pub done: bool,
    /// The user who created it.
    pub created_by: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub type Todo = Model;

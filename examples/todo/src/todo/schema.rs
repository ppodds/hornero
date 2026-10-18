use hornero::prelude::*;
use hornero_sea_orm::Db;
use sea_orm::{ConnectionTrait, Schema};

use super::entity;

/// Creates the table at startup through a lifecycle hook. Real applications would run
/// `sea-orm-migration` here instead.
#[derive(Injectable)]
#[inject(hooks)]
pub struct TodoSchema {
    db: Arc<Db>,
}

impl Lifecycle for TodoSchema {
    async fn on_module_init(&self) -> Result<(), BoxError> {
        let schema = Schema::new(self.db.get_database_backend());
        self.db
            .execute(
                schema
                    .create_table_from_entity(entity::Entity)
                    .if_not_exists(),
            )
            .await?;
        Ok(())
    }
}

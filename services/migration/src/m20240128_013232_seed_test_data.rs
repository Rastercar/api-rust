use sea_orm_migration::{prelude::*, sea_orm::TransactionTrait};

use crate::seeder;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let transaction = db.begin().await?;

        // maintain this order
        seeder::create_test_master_user(&transaction).await?;
        let test_user = seeder::create_test_user(&transaction).await?;

        seeder::create_entities_for_org(&transaction, test_user.organization_id.unwrap()).await?;

        for _ in 0..5 {
            seeder::root_user_with_user_org(&transaction).await.unwrap();
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(String::from("cannot be reverted")))
    }
}

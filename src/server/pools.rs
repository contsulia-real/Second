use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use super::ServerConfig;

/// The executor and prerequisite work domains intentionally use separate pools
/// while sharing one PostgreSQL database and one connection configuration.
#[derive(Clone, Debug)]
pub struct DatabasePools {
    pub execution: PgPool,
    pub prerequisite: PgPool,
}

pub async fn connect_database_pools(config: &ServerConfig) -> Result<DatabasePools, sqlx::Error> {
    let execution = PgPoolOptions::new()
        .max_connections(config.execution_pool_size())
        .connect_with(config.database().clone())
        .await?;

    let prerequisite = match PgPoolOptions::new()
        .max_connections(config.prerequisite_pool_size())
        .connect_with(config.database().clone())
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            execution.close().await;
            return Err(error);
        }
    };

    Ok(DatabasePools {
        execution,
        prerequisite,
    })
}

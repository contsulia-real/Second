use std::fmt;

use sqlx::PgPool;

use crate::VerifiedLegalTask;

/// Request identity is persisted as the protocol's actual legality proof, not a
/// side hash or second request identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskIdentityState {
    Fresh,
    KnownIncomplete,
    Completed,
}

#[derive(Debug)]
pub enum TaskIdentityError {
    Conflict,
    Database(sqlx::Error),
    CorruptLegalityProof,
}

impl fmt::Display for TaskIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict => f.write_str("task id is permanently bound to another signed request"),
            Self::Database(error) => write!(f, "database error: {error}"),
            Self::CorruptLegalityProof => {
                f.write_str("stored legality proof does not have the protocol length")
            }
        }
    }
}

impl std::error::Error for TaskIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Conflict | Self::CorruptLegalityProof => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PgTaskIdentityStore {
    pool: PgPool,
}

impl PgTaskIdentityStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Permanently binds a fresh task id before expiry/business execution and
    /// distinguishes an exact retry from an id conflict.
    pub async fn inspect_or_bind(
        &self,
        task: &VerifiedLegalTask,
    ) -> Result<TaskIdentityState, TaskIdentityError> {
        let task_id = task.task_id();
        let task_id_text = task_id.as_str();
        let legality_proof = task.legality_proof();

        let inserted = sqlx::query(
            "INSERT INTO task_identities (task_id, legality_proof) VALUES ($1, $2) \
             ON CONFLICT (task_id) DO NOTHING",
        )
        .bind(task_id_text)
        .bind(legality_proof.as_slice())
        .execute(&self.pool)
        .await
        .map_err(TaskIdentityError::Database)?
        .rows_affected()
            == 1;

        let stored: Vec<u8> =
            sqlx::query_scalar("SELECT legality_proof FROM task_identities WHERE task_id = $1")
                .bind(task_id_text)
                .fetch_one(&self.pool)
                .await
                .map_err(TaskIdentityError::Database)?;

        let stored: [u8; 64] = stored
            .try_into()
            .map_err(|_| TaskIdentityError::CorruptLegalityProof)?;
        if stored != legality_proof {
            return Err(TaskIdentityError::Conflict);
        }

        let completed: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM task_executions WHERE task_id = $1)")
                .bind(task_id_text)
                .fetch_one(&self.pool)
                .await
                .map_err(TaskIdentityError::Database)?;

        Ok(if completed {
            TaskIdentityState::Completed
        } else if inserted {
            TaskIdentityState::Fresh
        } else {
            TaskIdentityState::KnownIncomplete
        })
    }
}

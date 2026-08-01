use sqlx::PgPool;

use crate::error::ApiError;

#[derive(Clone)]
pub struct ApiState {
    pub pool: PgPool,
}

pub async fn materialise_due_now(pool: &PgPool) -> Result<(), ApiError> {
    tracked_worker::materialise::materialise_due(pool, chrono::Utc::now())
        .await
        .map(|_| ())
        .map_err(|error| ApiError::Worker(error.to_string()))
}

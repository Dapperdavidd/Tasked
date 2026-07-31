use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn set_request_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select set_config('app.user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut **tx)
        .await?;

    Ok(())
}

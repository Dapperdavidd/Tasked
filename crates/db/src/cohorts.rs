use chrono::NaiveDate;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::CohortPresenceRow;

pub async fn cohort_presence_on(
    tx: &mut Transaction<'_, Postgres>,
    cohort_id: Uuid,
    local_date: NaiveDate,
) -> Result<Vec<CohortPresenceRow>, sqlx::Error> {
    sqlx::query_as::<_, CohortPresenceRow>(
        r#"
        select cohort_id, user_id, display_name, avatar_url, streak, logged_today
        from cohort_presence_on($1, $2)
        order by logged_today desc, streak desc, display_name asc nulls last
        "#,
    )
    .bind(cohort_id)
    .bind(local_date)
    .fetch_all(&mut **tx)
    .await
}

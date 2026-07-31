use chrono::NaiveDate;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::RestDayRow;

pub async fn declare_rest_day(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    local_date: NaiveDate,
    reason: Option<&str>,
) -> Result<RestDayRow, sqlx::Error> {
    let id = Uuid::now_v7();

    sqlx::query_as::<_, RestDayRow>(
        r#"
        insert into rest_days (id, user_id, local_date, reason)
        values ($1, $2, $3, $4)
        on conflict (user_id, local_date)
        do update set reason = excluded.reason
        returning id, user_id, local_date, reason, created_at
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(local_date)
    .bind(reason)
    .fetch_one(&mut **tx)
    .await
}

pub async fn delete_rest_day(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    local_date: NaiveDate,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        delete from rest_days
        where user_id = $1
          and local_date = $2
        "#,
    )
    .bind(user_id)
    .bind(local_date)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub async fn recent_rest_days(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    from: NaiveDate,
) -> Result<Vec<RestDayRow>, sqlx::Error> {
    sqlx::query_as::<_, RestDayRow>(
        r#"
        select id, user_id, local_date, reason, created_at
        from rest_days
        where user_id = $1
          and local_date >= $2
        order by local_date desc
        "#,
    )
    .bind(user_id)
    .bind(from)
    .fetch_all(&mut **tx)
    .await
}

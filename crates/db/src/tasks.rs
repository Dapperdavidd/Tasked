use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::{DayRow, TaskInstanceRow};

pub async fn complete_task(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
    completed_at: DateTime<Utc>,
) -> Result<TaskInstanceRow, sqlx::Error> {
    let task = sqlx::query_as::<_, TaskInstanceRow>(
        r#"
        update task_instances
        set completed_at = coalesce(completed_at, $2),
            skipped_reason = null
        where id = $1
        returning id, day_id, template_id, title, points, position, is_floating,
                  completed_at, skipped_reason, proof_kind, proof_value
        "#,
    )
    .bind(task_id)
    .bind(completed_at)
    .fetch_one(&mut **tx)
    .await?;

    refresh_earned_points(tx, task.day_id).await?;
    Ok(task)
}

pub async fn uncomplete_task(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
) -> Result<TaskInstanceRow, sqlx::Error> {
    let task = sqlx::query_as::<_, TaskInstanceRow>(
        r#"
        update task_instances
        set completed_at = null
        where id = $1
        returning id, day_id, template_id, title, points, position, is_floating,
                  completed_at, skipped_reason, proof_kind, proof_value
        "#,
    )
    .bind(task_id)
    .fetch_one(&mut **tx)
    .await?;

    refresh_earned_points(tx, task.day_id).await?;
    Ok(task)
}

pub async fn skip_task(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
    reason: &str,
) -> Result<TaskInstanceRow, sqlx::Error> {
    let task = sqlx::query_as::<_, TaskInstanceRow>(
        r#"
        update task_instances
        set skipped_reason = $2,
            completed_at = null
        where id = $1
        returning id, day_id, template_id, title, points, position, is_floating,
                  completed_at, skipped_reason, proof_kind, proof_value
        "#,
    )
    .bind(task_id)
    .bind(reason)
    .fetch_one(&mut **tx)
    .await?;

    refresh_available_points(tx, task.day_id).await?;
    refresh_earned_points(tx, task.day_id).await?;
    Ok(task)
}

pub async fn add_proof(
    tx: &mut Transaction<'_, Postgres>,
    task_id: Uuid,
    proof_kind: &str,
    proof_value: &str,
) -> Result<TaskInstanceRow, sqlx::Error> {
    sqlx::query_as::<_, TaskInstanceRow>(
        r#"
        update task_instances
        set proof_kind = $2,
            proof_value = $3
        where id = $1
        returning id, day_id, template_id, title, points, position, is_floating,
                  completed_at, skipped_reason, proof_kind, proof_value
        "#,
    )
    .bind(task_id)
    .bind(proof_kind)
    .bind(proof_value)
    .fetch_one(&mut **tx)
    .await
}

pub async fn refresh_earned_points(
    tx: &mut Transaction<'_, Postgres>,
    day_id: Uuid,
) -> Result<DayRow, sqlx::Error> {
    sqlx::query_as::<_, DayRow>(
        r#"
        update days
        set earned_points = coalesce((
          select sum(points)::int
          from task_instances
          where day_id = $1
            and completed_at is not null
            and skipped_reason is null
        ), 0)
        where id = $1
        returning id, enrollment_id, local_date, day_index, status, available_points,
                  earned_points, note, opens_at, closes_at, finalised_at
        "#,
    )
    .bind(day_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn refresh_available_points(
    tx: &mut Transaction<'_, Postgres>,
    day_id: Uuid,
) -> Result<DayRow, sqlx::Error> {
    sqlx::query_as::<_, DayRow>(
        r#"
        update days
        set available_points = coalesce((
          select sum(points)::int
          from task_instances
          where day_id = $1
            and not is_floating
            and skipped_reason is null
        ), 0)
        where id = $1
        returning id, enrollment_id, local_date, day_index, status, available_points,
                  earned_points, note, opens_at, closes_at, finalised_at
        "#,
    )
    .bind(day_id)
    .fetch_one(&mut **tx)
    .await
}

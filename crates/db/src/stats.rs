use chrono::NaiveDate;
use sqlx::FromRow;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::{DayRow, TaskTemplateCompletionRow, WeekBucketRow};

pub async fn days_for_enrollment_range(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<DayRow>, sqlx::Error> {
    sqlx::query_as::<_, DayRow>(
        r#"
        select id, enrollment_id, local_date, day_index, status, available_points,
               earned_points, note, opens_at, closes_at, finalised_at
        from days
        where enrollment_id = $1
          and local_date >= $2
          and local_date <= $3
        order by local_date
        "#,
    )
    .bind(enrollment_id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **tx)
    .await
}

pub async fn week_buckets_for_enrollment_range(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    from_iso_year: i32,
    from_iso_week: i32,
    to_iso_year: i32,
    to_iso_week: i32,
) -> Result<Vec<WeekBucketRow>, sqlx::Error> {
    sqlx::query_as::<_, WeekBucketRow>(
        r#"
        select enrollment_id, iso_year, iso_week, template_id,
               required, completed, points_each
        from week_buckets
        where enrollment_id = $1
          and (iso_year, iso_week) >= ($2, $3)
          and (iso_year, iso_week) <= ($4, $5)
        order by iso_year, iso_week, template_id
        "#,
    )
    .bind(enrollment_id)
    .bind(from_iso_year)
    .bind(from_iso_week)
    .bind(to_iso_year)
    .bind(to_iso_week)
    .fetch_all(&mut **tx)
    .await
}

pub async fn per_task_completion_rates(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<TaskTemplateCompletionRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskTemplateCompletionRow>(
        r#"
        select ti.template_id,
               min(ti.title) as title,
               count(*) filter (where ti.skipped_reason is null) as available_count,
               count(*) filter (
                 where ti.completed_at is not null
                   and ti.skipped_reason is null
               ) as completed_count
        from task_instances ti
        join days d on d.id = ti.day_id
        where d.enrollment_id = $1
          and d.local_date >= $2
          and d.local_date <= $3
          and not ti.is_floating
        group by ti.template_id
        order by
          (count(*) filter (
             where ti.completed_at is not null
               and ti.skipped_reason is null
           )::float / nullif(count(*) filter (where ti.skipped_reason is null), 0)) asc nulls last,
          min(ti.title) asc
        "#,
    )
    .bind(enrollment_id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **tx)
    .await
}

pub async fn completed_task_counts_by_day(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<DayTaskCompletionCountRow>, sqlx::Error> {
    sqlx::query_as::<_, DayTaskCompletionCountRow>(
        r#"
        select d.id as day_id,
               count(ti.id) filter (
                 where ti.completed_at is not null
                   and ti.skipped_reason is null
               ) as completed_count,
               count(ti.id) filter (
                 where ti.skipped_reason is null
               ) as available_count
        from days d
        left join task_instances ti on ti.day_id = d.id
        where d.enrollment_id = $1
          and d.local_date >= $2
          and d.local_date <= $3
        group by d.id
        "#,
    )
    .bind(enrollment_id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut **tx)
    .await
}

#[derive(Clone, Debug, FromRow)]
pub struct DayTaskCompletionCountRow {
    pub day_id: Uuid,
    pub completed_count: Option<i64>,
    pub available_count: Option<i64>,
}

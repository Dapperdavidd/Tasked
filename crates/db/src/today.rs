use chrono::NaiveDate;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::{TaskInstanceRow, TodaySectionRow};

pub async fn sections_for_today(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    local_date: NaiveDate,
) -> Result<Vec<TodaySectionRow>, sqlx::Error> {
    sqlx::query_as::<_, TodaySectionRow>(
        r#"
        select e.id as enrollment_id,
               d.id as day_id,
               p.id as program_id,
               p.title,
               e.is_standing,
               d.day_index,
               p.duration_days,
               d.status as day_status,
               d.available_points,
               d.earned_points,
               d.note,
               s.current as streak_current,
               s.longest as streak_longest,
               s.freezes as streak_freezes,
               s.state as streak_state
        from enrollments e
        join programs p on p.id = e.program_id
        join days d on d.enrollment_id = e.id
        join streak_states s on s.enrollment_id = e.id
        where e.user_id = $1
          and e.status = 'active'
          and d.local_date = $2
        order by e.is_standing asc, e.created_at asc
        "#,
    )
    .bind(user_id)
    .bind(local_date)
    .fetch_all(&mut **tx)
    .await
}

pub async fn tasks_for_day(
    tx: &mut Transaction<'_, Postgres>,
    day_id: Uuid,
) -> Result<Vec<TaskInstanceRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskInstanceRow>(
        r#"
        select id, day_id, template_id, title, points, position, is_floating,
               completed_at, skipped_reason, proof_kind, proof_value
        from task_instances
        where day_id = $1
        order by
          (completed_at is not null) asc,
          position asc,
          id asc
        "#,
    )
    .bind(day_id)
    .fetch_all(&mut **tx)
    .await
}

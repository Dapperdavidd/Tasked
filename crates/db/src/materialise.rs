use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::{DayRow, EnrollmentRow, TaskInstanceRow, TaskTemplateRow};

pub async fn active_enrollments(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<EnrollmentRow>, sqlx::Error> {
    sqlx::query_as::<_, EnrollmentRow>(
        r#"
        select id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
               start_date, is_standing, status, materialised_through, created_at
        from enrollments
        where status = 'active'
        order by timezone, created_at
        "#,
    )
    .fetch_all(&mut **tx)
    .await
}

pub async fn active_templates_for_program(
    tx: &mut Transaction<'_, Postgres>,
    program_id: Uuid,
) -> Result<Vec<TaskTemplateRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskTemplateRow>(
        r#"
        select id, program_id, position, title, description, category, difficulty,
               estimated_minutes, cadence, points, paused_at, created_at
        from task_templates
        where program_id = $1
          and paused_at is null
        order by position
        "#,
    )
    .bind(program_id)
    .fetch_all(&mut **tx)
    .await
}

pub async fn upsert_day(
    tx: &mut Transaction<'_, Postgres>,
    new_day: NewDay,
) -> Result<DayRow, sqlx::Error> {
    sqlx::query_as::<_, DayRow>(
        r#"
        insert into days (
          id,
          enrollment_id,
          local_date,
          day_index,
          opens_at,
          closes_at
        )
        values ($1, $2, $3, $4, $5, $6)
        on conflict (enrollment_id, local_date)
        do update set
          opens_at = days.opens_at,
          closes_at = days.closes_at
        returning id, enrollment_id, local_date, day_index, status, available_points,
                  earned_points, note, opens_at, closes_at, finalised_at
        "#,
    )
    .bind(new_day.id)
    .bind(new_day.enrollment_id)
    .bind(new_day.local_date)
    .bind(new_day.day_index)
    .bind(new_day.opens_at)
    .bind(new_day.closes_at)
    .fetch_one(&mut **tx)
    .await
}

pub async fn upsert_task_instance(
    tx: &mut Transaction<'_, Postgres>,
    new_task: NewTaskInstance<'_>,
) -> Result<TaskInstanceRow, sqlx::Error> {
    sqlx::query_as::<_, TaskInstanceRow>(
        r#"
        insert into task_instances (
          id,
          day_id,
          template_id,
          title,
          points,
          position,
          is_floating
        )
        values ($1, $2, $3, $4, $5, $6, $7)
        on conflict (day_id, template_id)
        do update set
          title = task_instances.title,
          points = task_instances.points,
          position = task_instances.position,
          is_floating = task_instances.is_floating
        returning id, day_id, template_id, title, points, position, is_floating,
                  completed_at, skipped_reason, proof_kind, proof_value
        "#,
    )
    .bind(new_task.id)
    .bind(new_task.day_id)
    .bind(new_task.template_id)
    .bind(new_task.title)
    .bind(new_task.points)
    .bind(new_task.position)
    .bind(new_task.is_floating)
    .fetch_one(&mut **tx)
    .await
}

pub async fn refresh_day_available_points(
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

pub async fn mark_materialised_through(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    through: NaiveDate,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        update enrollments
        set materialised_through = greatest(coalesce(materialised_through, $2), $2)
        where id = $1
        "#,
    )
    .bind(enrollment_id)
    .bind(through)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct NewDay {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub local_date: NaiveDate,
    pub day_index: i32,
    pub opens_at: DateTime<Utc>,
    pub closes_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug)]
pub struct NewTaskInstance<'a> {
    pub id: Uuid,
    pub day_id: Uuid,
    pub template_id: Uuid,
    pub title: &'a str,
    pub points: i32,
    pub position: i32,
    pub is_floating: bool,
}

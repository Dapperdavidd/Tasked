use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::{EnrollmentRow, ProgramRow, TaskTemplateRow};

pub async fn standing_enrollment_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<EnrollmentRow, sqlx::Error> {
    sqlx::query_as::<_, EnrollmentRow>(
        r#"
        select id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
               start_date, is_standing, status, materialised_through, created_at
        from enrollments
        where user_id = $1
          and is_standing
        "#,
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn standing_program_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<ProgramRow, sqlx::Error> {
    sqlx::query_as::<_, ProgramRow>(
        r#"
        select p.id, p.author_id, p.title, p.summary, p.kind, p.duration_days,
               p.intensity, p.source_id, p.share_titles, p.created_at
        from programs p
        join enrollments e on e.program_id = p.id
        where e.user_id = $1
          and e.is_standing
        "#,
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn active_standing_templates(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<TaskTemplateRow>, sqlx::Error> {
    sqlx::query_as::<_, TaskTemplateRow>(
        r#"
        select t.id, t.program_id, t.position, t.title, t.description, t.category,
               t.difficulty, t.estimated_minutes, t.cadence, t.points,
               t.paused_at, t.created_at
        from task_templates t
        join enrollments e on e.program_id = t.program_id
        where e.user_id = $1
          and e.is_standing
          and t.paused_at is null
        order by t.position
        "#,
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await
}

pub async fn active_standing_count(
    tx: &mut Transaction<'_, Postgres>,
    program_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        r#"
        select count(*)
        from task_templates
        where program_id = $1
          and paused_at is null
        "#,
    )
    .bind(program_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn create_standing_template<T>(
    tx: &mut Transaction<'_, Postgres>,
    new_template: NewStandingTemplate<'_>,
    cadence: &T,
) -> Result<TaskTemplateRow, sqlx::Error>
where
    T: Serialize,
{
    let cadence =
        serde_json::to_value(cadence).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;

    sqlx::query_as::<_, TaskTemplateRow>(
        r#"
        insert into task_templates (
          id,
          program_id,
          position,
          title,
          description,
          category,
          difficulty,
          estimated_minutes,
          cadence,
          points
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        returning id, program_id, position, title, description, category,
                  difficulty, estimated_minutes, cadence, points, paused_at, created_at
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(new_template.program_id)
    .bind(new_template.position)
    .bind(new_template.title)
    .bind(new_template.description)
    .bind(new_template.category)
    .bind(new_template.difficulty)
    .bind(new_template.estimated_minutes)
    .bind(cadence)
    .bind(new_template.points)
    .fetch_one(&mut **tx)
    .await
}

pub async fn pause_standing_template(
    tx: &mut Transaction<'_, Postgres>,
    template_id: Uuid,
) -> Result<TaskTemplateRow, sqlx::Error> {
    sqlx::query_as::<_, TaskTemplateRow>(
        r#"
        update task_templates
        set paused_at = now()
        where id = $1
        returning id, program_id, position, title, description, category,
                  difficulty, estimated_minutes, cadence, points, paused_at, created_at
        "#,
    )
    .bind(template_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn replace_standing_template<T>(
    tx: &mut Transaction<'_, Postgres>,
    out_template_id: Uuid,
    new_template: NewStandingTemplate<'_>,
    cadence: &T,
) -> Result<TaskTemplateRow, sqlx::Error>
where
    T: Serialize,
{
    pause_standing_template(tx, out_template_id).await?;
    create_standing_template(tx, new_template, cadence).await
}

#[derive(Clone, Copy, Debug)]
pub struct NewStandingTemplate<'a> {
    pub program_id: Uuid,
    pub position: i32,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub category: Option<&'a str>,
    pub difficulty: i16,
    pub estimated_minutes: i32,
    pub points: i32,
}

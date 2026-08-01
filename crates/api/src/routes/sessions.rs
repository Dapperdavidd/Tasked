use actix_web::{post, web, HttpResponse};
use chrono::Utc;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use tracked_core::scoring;
use tracked_db::rls;
use uuid::Uuid;

use crate::{
    app::{materialise_due_now, ApiState},
    error::ApiError,
};

#[derive(Deserialize)]
pub struct CreateSessionBody {
    timezone: Option<String>,
    display_name: Option<String>,
}

#[post("/v1/sessions")]
pub async fn create_session(
    state: web::Data<ApiState>,
    body: web::Json<CreateSessionBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;

    let user_id = Uuid::now_v7();
    let standing_program_id = Uuid::now_v7();
    let standing_enrollment_id = Uuid::now_v7();
    let timezone = body
        .timezone
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Africa/Lagos".to_owned());
    let display_name = body
        .display_name
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Dapper".to_owned());
    let email = format!("anon-{user_id}@tracked.local");

    sqlx::query(
        r#"
        insert into users (id, email, display_name)
        values ($1, $2, $3)
        "#,
    )
    .bind(user_id)
    .bind(email)
    .bind(&display_name)
    .execute(&mut *tx)
    .await?;

    rls::set_request_user(&mut tx, user_id).await?;

    sqlx::query(
        r#"
        insert into user_settings (
          user_id, timezone, day_boundary_hour, morning_at, evening_at, locale
        )
        values ($1, $2, 0, '07:30', '20:30', 'en')
        "#,
    )
    .bind(user_id)
    .bind(&timezone)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        insert into programs (
          id, author_id, title, summary, kind, duration_days, intensity, source_id
        )
        values ($1, $2, 'Standing List', 'Private capped baseline tasks.', 'standing', null, null, null)
        "#,
    )
    .bind(standing_program_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    let tz = timezone.parse::<Tz>().map_err(|_| ApiError::InvalidUser)?;
    let start_date = Utc::now().with_timezone(&tz).date_naive();
    sqlx::query(
        r#"
        insert into enrollments (
          id, user_id, program_id, timezone, day_boundary_hour,
          start_date, is_standing, status
        )
        values ($1, $2, $3, $4, 0, $5, true, 'active')
        "#,
    )
    .bind(standing_enrollment_id)
    .bind(user_id)
    .bind(standing_program_id)
    .bind(&timezone)
    .bind(start_date)
    .execute(&mut *tx)
    .await?;

    sqlx::query("insert into streak_states (enrollment_id) values ($1)")
        .bind(standing_enrollment_id)
        .execute(&mut *tx)
        .await?;

    seed_standing_template(&mut tx, standing_program_id, 1, "Take vitamins", 5).await?;
    seed_standing_template(
        &mut tx,
        standing_program_id,
        2,
        "Meditate for 10 minutes",
        10,
    )
    .await?;

    tx.commit().await?;
    materialise_due_now(&state.pool).await?;

    Ok(HttpResponse::Ok().json(SessionResponse {
        user_id,
        display_name,
        timezone,
    }))
}

async fn seed_standing_template(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    program_id: Uuid,
    position: i32,
    title: &str,
    minutes: u16,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        insert into task_templates (
          id, program_id, position, title, difficulty, estimated_minutes,
          cadence, points
        )
        values ($1, $2, $3, $4, 1, $5, '{"type":"daily"}'::jsonb, $6)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(program_id)
    .bind(position)
    .bind(title)
    .bind(i32::from(minutes))
    .bind(scoring::task_points(1, minutes))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[derive(Serialize)]
struct SessionResponse {
    user_id: Uuid,
    display_name: String,
    timezone: String,
}

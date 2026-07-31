use actix_web::{get, patch, web, HttpResponse};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tracked_db::{rls, rows::EnrollmentRow};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[get("/v1/enrollments")]
pub async fn list_enrollments(
    state: web::Data<ApiState>,
    user_id: UserId,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let rows = sqlx::query_as::<_, EnrollmentRow>(
        r#"
        select id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
               start_date, is_standing, status, materialised_through, created_at
        from enrollments
        where user_id = $1
        order by is_standing asc, created_at desc
        "#,
    )
    .bind(user_id.0)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(
        rows.into_iter()
            .map(EnrollmentResponse::from)
            .collect::<Vec<_>>(),
    ))
}

#[derive(Deserialize)]
pub struct PatchEnrollmentBody {
    status: Option<String>,
    timezone: Option<String>,
    day_boundary_hour: Option<i16>,
}

#[patch("/v1/enrollments/{id}")]
pub async fn patch_enrollment(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
    body: web::Json<PatchEnrollmentBody>,
) -> Result<HttpResponse, ApiError> {
    if let Some(hour) = body.day_boundary_hour {
        if !(0..=4).contains(&hour) {
            return Err(ApiError::BadRequest("invalid day boundary hour".to_owned()));
        }
    }
    if let Some(status) = &body.status {
        if !matches!(
            status.as_str(),
            "active" | "paused" | "completed" | "abandoned"
        ) {
            return Err(ApiError::BadRequest("invalid enrollment status".to_owned()));
        }
    }

    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let row = sqlx::query_as::<_, EnrollmentRow>(
        r#"
        update enrollments
        set status = coalesce($3, status),
            timezone = coalesce($4, timezone),
            day_boundary_hour = coalesce($5, day_boundary_hour)
        where id = $1
          and user_id = $2
        returning id, user_id, program_id, cohort_id, timezone, day_boundary_hour,
                  start_date, is_standing, status, materialised_through, created_at
        "#,
    )
    .bind(*path)
    .bind(user_id.0)
    .bind(&body.status)
    .bind(&body.timezone)
    .bind(body.day_boundary_hour)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(EnrollmentResponse::from(row)))
}

#[derive(Serialize)]
struct EnrollmentResponse {
    id: Uuid,
    program_id: Uuid,
    cohort_id: Option<Uuid>,
    timezone: String,
    day_boundary_hour: i16,
    start_date: NaiveDate,
    is_standing: bool,
    status: String,
    materialised_through: Option<NaiveDate>,
}

impl From<EnrollmentRow> for EnrollmentResponse {
    fn from(row: EnrollmentRow) -> Self {
        Self {
            id: row.id,
            program_id: row.program_id,
            cohort_id: row.cohort_id,
            timezone: row.timezone,
            day_boundary_hour: row.day_boundary_hour,
            start_date: row.start_date,
            is_standing: row.is_standing,
            status: format!("{:?}", row.status),
            materialised_through: row.materialised_through,
        }
    }
}

use actix_web::{get, patch, web, HttpResponse};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tracked_db::{rls, rows::DayRow, stats as stats_db};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct DaysQuery {
    enrollment: Uuid,
    from: NaiveDate,
    to: NaiveDate,
}

#[get("/v1/days")]
pub async fn get_days(
    state: web::Data<ApiState>,
    user_id: UserId,
    query: web::Query<DaysQuery>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let days = stats_db::days_for_enrollment_range(&mut tx, query.enrollment, query.from, query.to)
        .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(days.into_iter().map(DayResponse::from).collect::<Vec<_>>()))
}

#[derive(Deserialize)]
pub struct PatchDayBody {
    note: Option<String>,
}

#[patch("/v1/days/{id}")]
pub async fn patch_day(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
    body: web::Json<PatchDayBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let row = sqlx::query_as::<_, DayRow>(
        r#"
        update days
        set note = $2
        where id = $1
        returning id, enrollment_id, local_date, day_index, status, available_points,
                  earned_points, note, opens_at, closes_at, finalised_at
        "#,
    )
    .bind(*path)
    .bind(&body.note)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(DayResponse::from(row)))
}

#[derive(Serialize)]
struct DayResponse {
    id: Uuid,
    enrollment_id: Uuid,
    local_date: NaiveDate,
    day_index: i32,
    status: String,
    available_points: i32,
    earned_points: i32,
    note: Option<String>,
}

impl From<DayRow> for DayResponse {
    fn from(row: DayRow) -> Self {
        Self {
            id: row.id,
            enrollment_id: row.enrollment_id,
            local_date: row.local_date,
            day_index: row.day_index,
            status: format!("{:?}", row.status),
            available_points: row.available_points,
            earned_points: row.earned_points,
            note: row.note,
        }
    }
}

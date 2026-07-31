use actix_web::{delete, post, web, HttpResponse};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tracked_db::{rest_days as rest_days_db, rls};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct RestDayBody {
    local_date: NaiveDate,
    reason: Option<String>,
}

#[post("/v1/rest-days")]
pub async fn declare_rest_day(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<RestDayBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let row =
        rest_days_db::declare_rest_day(&mut tx, user_id.0, body.local_date, body.reason.as_deref())
            .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(RestDayResponse::from(row)))
}

#[delete("/v1/rest-days/{local_date}")]
pub async fn delete_rest_day(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<NaiveDate>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    rest_days_db::delete_rest_day(&mut tx, user_id.0, *path).await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}

#[derive(Serialize)]
struct RestDayResponse {
    id: Uuid,
    local_date: NaiveDate,
    reason: Option<String>,
}

impl From<tracked_db::rows::RestDayRow> for RestDayResponse {
    fn from(row: tracked_db::rows::RestDayRow) -> Self {
        Self {
            id: row.id,
            local_date: row.local_date,
            reason: row.reason,
        }
    }
}

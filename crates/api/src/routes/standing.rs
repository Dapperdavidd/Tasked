use actix_web::{get, web, HttpResponse};
use serde::Serialize;
use serde_json::Value;
use tracked_db::{rls, standing as standing_db};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[get("/v1/standing")]
pub async fn get_standing(
    state: web::Data<ApiState>,
    user_id: UserId,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let templates = standing_db::active_standing_templates(&mut tx, user_id.0).await?;
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(
        templates
            .into_iter()
            .map(|template| StandingTaskResponse {
                id: template.id,
                title: template.title,
                position: template.position,
                estimated_minutes: template.estimated_minutes,
                cadence: template.cadence,
                points: template.points,
            })
            .collect::<Vec<_>>(),
    ))
}

#[derive(Serialize)]
struct StandingTaskResponse {
    id: Uuid,
    title: String,
    position: i32,
    estimated_minutes: i32,
    cadence: Value,
    points: i32,
}

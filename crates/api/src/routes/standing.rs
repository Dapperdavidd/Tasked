use actix_web::{get, post, web, HttpResponse};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracked_core::scoring;
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

#[derive(Deserialize)]
pub struct CreateStandingBody {
    title: String,
    cadence: StandingCadenceBody,
    duration_bucket: DurationBucket,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StandingCadenceBody {
    Daily,
    WeeklyDays { days: Vec<u8> },
    NPerWeek { count: u8 },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DurationBucket {
    Five,
    Ten,
    Fifteen,
    Thirty,
}

#[post("/v1/standing")]
pub async fn create_standing(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<CreateStandingBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let standing = standing_db::standing_program_for_user(&mut tx, user_id.0).await?;
    let count = standing_db::active_standing_count(&mut tx, standing.id).await?;
    if count >= 5 {
        return Err(ApiError::BadRequest("standing task cap reached".to_owned()));
    }
    let position = i32::try_from(count + 1)
        .map_err(|_| ApiError::BadRequest("standing position out of range".to_owned()))?;
    let estimated_minutes = duration_minutes(&body.duration_bucket);
    let points = scoring::task_points(1, estimated_minutes as u16);
    let cadence = cadence_json(&body.cadence)?;
    let task = standing_db::create_standing_template(
        &mut tx,
        standing_db::NewStandingTemplate {
            program_id: standing.id,
            position,
            title: &body.title,
            description: None,
            category: None,
            difficulty: 1,
            estimated_minutes,
            points,
        },
        &cadence,
    )
    .await?;
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(StandingTaskResponse {
        id: task.id,
        title: task.title,
        position: task.position,
        estimated_minutes: task.estimated_minutes,
        cadence: task.cadence,
        points: task.points,
    }))
}

#[post("/v1/standing/{id}/pause")]
pub async fn pause_standing(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let task = standing_db::pause_standing_template(&mut tx, *path).await?;
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(StandingTaskResponse {
        id: task.id,
        title: task.title,
        position: task.position,
        estimated_minutes: task.estimated_minutes,
        cadence: task.cadence,
        points: task.points,
    }))
}

fn duration_minutes(bucket: &DurationBucket) -> i32 {
    match bucket {
        DurationBucket::Five => 5,
        DurationBucket::Ten => 10,
        DurationBucket::Fifteen => 15,
        DurationBucket::Thirty => 30,
    }
}

fn cadence_json(cadence: &StandingCadenceBody) -> Result<Value, ApiError> {
    match cadence {
        StandingCadenceBody::Daily => Ok(json!({ "type": "daily" })),
        StandingCadenceBody::WeeklyDays { days } => {
            if days.is_empty() || days.iter().any(|day| !(1..=7).contains(day)) {
                return Err(ApiError::BadRequest("invalid weekday".to_owned()));
            }
            Ok(json!({ "type": "weekly_days", "days": days }))
        }
        StandingCadenceBody::NPerWeek { count } => {
            if !(1..=7).contains(count) {
                return Err(ApiError::BadRequest("invalid weekly count".to_owned()));
            }
            Ok(json!({ "type": "n_per_week", "count": count }))
        }
    }
}

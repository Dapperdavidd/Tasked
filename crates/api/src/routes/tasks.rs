use actix_web::{delete, post, web, HttpResponse};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracked_db::{rls, tasks as tasks_db};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct CompleteTaskBody {
    completed_at: Option<chrono::DateTime<Utc>>,
}

#[post("/v1/tasks/{id}/complete")]
pub async fn complete_task(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
    body: web::Json<CompleteTaskBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let task =
        tasks_db::complete_task(&mut tx, *path, body.completed_at.unwrap_or_else(Utc::now)).await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(TaskMutationResponse::from(task)))
}

#[delete("/v1/tasks/{id}/complete")]
pub async fn uncomplete_task(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let task = tasks_db::uncomplete_task(&mut tx, *path).await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(TaskMutationResponse::from(task)))
}

#[derive(Deserialize)]
pub struct SkipTaskBody {
    reason: String,
}

#[post("/v1/tasks/{id}/skip")]
pub async fn skip_task(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
    body: web::Json<SkipTaskBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let task = tasks_db::skip_task(&mut tx, *path, &body.reason).await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(TaskMutationResponse::from(task)))
}

#[derive(Serialize)]
struct TaskMutationResponse {
    id: Uuid,
    day_id: Uuid,
    title: String,
    completed_at: Option<chrono::DateTime<Utc>>,
    skipped_reason: Option<String>,
}

impl From<tracked_db::rows::TaskInstanceRow> for TaskMutationResponse {
    fn from(row: tracked_db::rows::TaskInstanceRow) -> Self {
        Self {
            id: row.id,
            day_id: row.day_id,
            title: row.title,
            completed_at: row.completed_at,
            skipped_reason: row.skipped_reason,
        }
    }
}

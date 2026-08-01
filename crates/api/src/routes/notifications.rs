use actix_web::{get, post, web, HttpResponse};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracked_db::{notifications as notifications_db, rls};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct TestNotificationBody {
    title: Option<String>,
    body: Option<String>,
}

#[post("/v1/notifications/test")]
pub async fn enqueue_test_notification(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<TestNotificationBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;

    let title = body.title.as_deref().unwrap_or("Tracked");
    let body_text = body
        .body
        .as_deref()
        .unwrap_or("Notifications are connected.");
    let event = notifications_db::insert_event(
        &mut tx,
        notifications_db::NewNotificationEvent {
            id: Uuid::now_v7(),
            user_id: user_id.0,
            kind: "morning_card",
            scheduled_at: Utc::now(),
            title,
            body: body_text,
            payload: json!({ "source": "test" }),
            status: "queued",
            skipped_reason: None,
        },
    )
    .await?
    .ok_or_else(|| ApiError::BadRequest("notification already queued".to_owned()))?;

    tx.commit().await?;
    Ok(HttpResponse::Ok().json(NotificationEventResponse::from(event)))
}

#[get("/v1/notifications")]
pub async fn list_notifications(
    state: web::Data<ApiState>,
    user_id: UserId,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let events = notifications_db::events_for_user(&mut tx, user_id.0, 50).await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(
        events
            .into_iter()
            .map(NotificationEventResponse::from)
            .collect::<Vec<_>>(),
    ))
}

#[derive(Serialize)]
struct NotificationEventResponse {
    id: Uuid,
    kind: String,
    scheduled_at: chrono::DateTime<Utc>,
    title: String,
    body: String,
    payload: serde_json::Value,
    status: String,
    skipped_reason: Option<String>,
    sent_at: Option<chrono::DateTime<Utc>>,
}

impl From<tracked_db::rows::NotificationEventRow> for NotificationEventResponse {
    fn from(row: tracked_db::rows::NotificationEventRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind,
            scheduled_at: row.scheduled_at,
            title: row.title,
            body: row.body,
            payload: row.payload,
            status: row.status,
            skipped_reason: row.skipped_reason,
            sent_at: row.sent_at,
        }
    }
}

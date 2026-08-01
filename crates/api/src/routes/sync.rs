use actix_web::{post, web, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tracked_db::{rest_days as rest_days_db, rls, tasks as tasks_db};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct SyncBody {
    mutations: Vec<SyncMutation>,
}

#[derive(Deserialize)]
pub struct SyncMutation {
    client_id: Uuid,
    client_timestamp: DateTime<Utc>,
    #[serde(flatten)]
    kind: SyncMutationKind,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SyncMutationKind {
    CompleteTask {
        task_id: Uuid,
        completed_at: Option<DateTime<Utc>>,
    },
    UncompleteTask {
        task_id: Uuid,
    },
    SkipTask {
        task_id: Uuid,
        reason: String,
    },
    PatchDayNote {
        day_id: Uuid,
        note: Option<String>,
    },
    DeclareRestDay {
        local_date: NaiveDate,
        reason: Option<String>,
    },
}

#[post("/v1/sync")]
pub async fn sync(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<SyncBody>,
) -> Result<HttpResponse, ApiError> {
    let mut mutations = body.mutations.iter().collect::<Vec<_>>();
    mutations.sort_by_key(|mutation| mutation.client_timestamp);

    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;

    let mut applied = Vec::with_capacity(mutations.len());
    let mut affected_days = Vec::new();

    for mutation in mutations {
        match &mutation.kind {
            SyncMutationKind::CompleteTask {
                task_id,
                completed_at,
            } => {
                let task = tasks_db::complete_task(
                    &mut tx,
                    *task_id,
                    completed_at.unwrap_or(mutation.client_timestamp),
                )
                .await?;
                affected_days.push(task.day_id);
            }
            SyncMutationKind::UncompleteTask { task_id } => {
                let task = tasks_db::uncomplete_task(&mut tx, *task_id).await?;
                affected_days.push(task.day_id);
            }
            SyncMutationKind::SkipTask { task_id, reason } => {
                let task = tasks_db::skip_task(&mut tx, *task_id, reason).await?;
                affected_days.push(task.day_id);
            }
            SyncMutationKind::PatchDayNote { day_id, note } => {
                sqlx::query("update days set note = $2 where id = $1")
                    .bind(*day_id)
                    .bind(note)
                    .execute(&mut *tx)
                    .await?;
                affected_days.push(*day_id);
            }
            SyncMutationKind::DeclareRestDay { local_date, reason } => {
                rest_days_db::declare_rest_day(&mut tx, user_id.0, *local_date, reason.as_deref())
                    .await?;
            }
        }
        applied.push(mutation.client_id);
    }

    affected_days.sort();
    affected_days.dedup();
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(SyncResponse {
        applied,
        affected_days,
    }))
}

#[derive(Serialize)]
struct SyncResponse {
    applied: Vec<Uuid>,
    affected_days: Vec<Uuid>,
}

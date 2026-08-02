use actix_web::{get, web, HttpResponse};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracked_db::{rls, stats as stats_db};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct StatsQuery {
    enrollment: Uuid,
    from: NaiveDate,
    to: NaiveDate,
}

#[get("/v1/stats")]
pub async fn stats(
    state: web::Data<ApiState>,
    user_id: UserId,
    query: web::Query<StatsQuery>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let days = stats_db::days_for_enrollment_range(&mut tx, query.enrollment, query.from, query.to)
        .await?;
    let day_counts =
        stats_db::completed_task_counts_by_day(&mut tx, query.enrollment, query.from, query.to)
            .await?
            .into_iter()
            .map(|row| {
                (
                    row.day_id,
                    (
                        row.completed_count.unwrap_or_default(),
                        row.available_count.unwrap_or_default(),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
    let tasks =
        stats_db::per_task_completion_rates(&mut tx, query.enrollment, query.from, query.to)
            .await?;
    tx.commit().await?;

    Ok(HttpResponse::Ok().json(StatsResponse {
        days: days
            .into_iter()
            .map(|day| DayStatsResponse {
                completed_tasks: day_counts
                    .get(&day.id)
                    .map(|(completed, _)| *completed)
                    .unwrap_or_default(),
                available_tasks: day_counts
                    .get(&day.id)
                    .map(|(_, available)| *available)
                    .unwrap_or_default(),
                id: day.id,
                local_date: day.local_date,
                status: format!("{:?}", day.status),
                available_points: day.available_points,
                earned_points: day.earned_points,
            })
            .collect(),
        tasks: tasks
            .into_iter()
            .map(|task| TaskCompletionResponse {
                template_id: task.template_id,
                title: task.title,
                available_count: task.available_count.unwrap_or_default(),
                completed_count: task.completed_count.unwrap_or_default(),
            })
            .collect(),
    }))
}

#[derive(Serialize)]
struct StatsResponse {
    days: Vec<DayStatsResponse>,
    tasks: Vec<TaskCompletionResponse>,
}

#[derive(Serialize)]
struct DayStatsResponse {
    id: Uuid,
    local_date: NaiveDate,
    status: String,
    available_points: i32,
    earned_points: i32,
    completed_tasks: i64,
    available_tasks: i64,
}

#[derive(Serialize)]
struct TaskCompletionResponse {
    template_id: Uuid,
    title: String,
    available_count: i64,
    completed_count: i64,
}

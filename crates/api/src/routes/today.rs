use actix_web::{get, web, HttpResponse};
use chrono::Utc;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use tracked_core::calendar;
use tracked_db::{rls, today as today_db};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct TodayQuery {
    local_date: Option<chrono::NaiveDate>,
}

#[get("/v1/today")]
pub async fn today(
    state: web::Data<ApiState>,
    user_id: UserId,
    query: web::Query<TodayQuery>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;

    let local_date = match query.local_date {
        Some(local_date) => local_date,
        None => user_local_today(&mut tx, user_id.0).await?,
    };

    let sections = today_db::sections_for_today(&mut tx, user_id.0, local_date).await?;
    let mut response_sections = Vec::with_capacity(sections.len());

    for section in sections {
        let tasks = today_db::tasks_for_day(&mut tx, section.day_id).await?;
        response_sections.push(TodaySectionResponse {
            enrollment_id: section.enrollment_id,
            day_id: section.day_id,
            kind: if section.is_standing {
                SectionKind::Standing
            } else {
                SectionKind::Program
            },
            title: section.title,
            day_index: (!section.is_standing).then_some(section.day_index),
            duration_days: section.duration_days,
            available_points: section.available_points,
            earned_points: section.earned_points,
            note: section.note,
            streak: StreakResponse {
                current: section.streak_current,
                longest: section.streak_longest,
                freezes: section.streak_freezes,
                state: format!("{:?}", section.streak_state),
            },
            tasks: tasks
                .into_iter()
                .map(|task| TaskResponse {
                    id: task.id,
                    title: task.title,
                    points: task.points,
                    position: task.position,
                    is_floating: task.is_floating,
                    completed_at: task.completed_at,
                    skipped_reason: task.skipped_reason,
                })
                .collect(),
        });
    }

    tx.commit().await?;

    Ok(HttpResponse::Ok().json(TodayResponse {
        local_date,
        sections: response_sections,
    }))
}

async fn user_local_today(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<chrono::NaiveDate, ApiError> {
    let (timezone, boundary): (String, i16) =
        sqlx::query_as("select timezone, day_boundary_hour from user_settings where user_id = $1")
            .bind(user_id)
            .fetch_one(&mut **tx)
            .await?;
    let tz = timezone.parse::<Tz>().map_err(|_| ApiError::InvalidUser)?;
    calendar::enrollment_today(Utc::now(), boundary as u32, tz).map_err(|_| ApiError::InvalidUser)
}

#[derive(Serialize)]
struct TodayResponse {
    local_date: chrono::NaiveDate,
    sections: Vec<TodaySectionResponse>,
}

#[derive(Serialize)]
enum SectionKind {
    Program,
    Standing,
}

#[derive(Serialize)]
struct TodaySectionResponse {
    enrollment_id: Uuid,
    day_id: Uuid,
    kind: SectionKind,
    title: String,
    day_index: Option<i32>,
    duration_days: Option<i32>,
    available_points: i32,
    earned_points: i32,
    note: Option<String>,
    streak: StreakResponse,
    tasks: Vec<TaskResponse>,
}

#[derive(Serialize)]
struct StreakResponse {
    current: i32,
    longest: i32,
    freezes: i16,
    state: String,
}

#[derive(Serialize)]
struct TaskResponse {
    id: Uuid,
    title: String,
    points: i32,
    position: i32,
    is_floating: bool,
    completed_at: Option<chrono::DateTime<Utc>>,
    skipped_reason: Option<String>,
}

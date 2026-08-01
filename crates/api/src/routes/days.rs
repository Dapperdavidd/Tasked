use actix_web::{get, patch, post, web, HttpResponse};
use chrono::{Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tracked_core::streak::{self, DayOutcome, Streak};
use tracked_db::{
    finalise as finalise_db, rls,
    rows::{DayRow, DayStatus, StreakState},
    stats as stats_db,
};
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

#[post("/v1/days/{id}/repair")]
pub async fn repair_day(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let now = Utc::now();
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;

    let day = finalise_db::day_for_update(&mut tx, *path).await?;
    if day.finalised_at.is_none() {
        return Err(ApiError::BadRequest("day is not finalised".to_owned()));
    }
    if day.status != DayStatus::Missed {
        return Err(ApiError::BadRequest(
            "only missed days can be repaired".to_owned(),
        ));
    }
    if now > day.closes_at + Duration::hours(24) {
        return Err(ApiError::BadRequest("repair window has closed".to_owned()));
    }

    finalise_db::lock_enrollment(&mut tx, day.enrollment_id).await?;
    finalise_db::complete_remaining_day_tasks(&mut tx, day.id, now).await?;
    let repaired = finalise_db::refresh_repaired_day(&mut tx, day.id).await?;

    let replay_days = finalise_db::finalised_days_for_replay(&mut tx, day.enrollment_id).await?;
    let outcomes = replay_days
        .iter()
        .map(outcome_from_day)
        .collect::<Result<Vec<_>, _>>()?;
    let streak = streak::fold(&outcomes);
    let last_counted_date = replay_days.last().map(|day| day.local_date);

    finalise_db::update_streak_state(
        &mut tx,
        new_replayed_streak_state(
            day.enrollment_id,
            &streak,
            last_counted_date,
            Some(day.local_date),
        )?,
    )
    .await?;

    tx.commit().await?;
    Ok(HttpResponse::Ok().json(DayResponse::from(repaired)))
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

fn outcome_from_day(day: &DayRow) -> Result<DayOutcome, ApiError> {
    match day.status {
        DayStatus::Complete => Ok(DayOutcome::Complete),
        DayStatus::Partial => Ok(DayOutcome::Partial),
        DayStatus::Missed => Ok(DayOutcome::Missed),
        DayStatus::Rest | DayStatus::Frozen => Ok(DayOutcome::Rest),
        DayStatus::Open => Err(ApiError::BadRequest(
            "open day cannot be replayed into streak".to_owned(),
        )),
    }
}

fn new_replayed_streak_state(
    enrollment_id: Uuid,
    streak: &Streak,
    last_counted_date: Option<NaiveDate>,
    repair_used_month: Option<NaiveDate>,
) -> Result<finalise_db::NewStreakState, ApiError> {
    Ok(finalise_db::NewStreakState {
        enrollment_id,
        current: u32_to_i32(streak.current)?,
        longest: u32_to_i32(streak.longest)?,
        freezes: u8_to_i16(streak.freezes),
        clean_run: u8_to_i16(streak.clean_run),
        last_counted_date,
        repair_used_month,
        state: db_streak_state(streak.state),
    })
}

fn db_streak_state(state: streak::StreakState) -> StreakState {
    match state {
        streak::StreakState::Active => StreakState::Active,
        streak::StreakState::AtRisk => StreakState::AtRisk,
        streak::StreakState::Repairable => StreakState::Repairable,
        streak::StreakState::Broken => StreakState::Broken,
    }
}

fn u32_to_i32(value: u32) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::BadRequest("streak value out of range".to_owned()))
}

fn u8_to_i16(value: u8) -> i16 {
    i16::from(value)
}

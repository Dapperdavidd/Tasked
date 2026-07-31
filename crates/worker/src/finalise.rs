use chrono::{Datelike, Duration, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use tracked_core::{
    scoring::{self, DayStatus as CoreDayStatus},
    streak::{self, DayOutcome, Finalisation, Streak},
};
use tracked_db::{
    finalise as db_finalise,
    jobs::{self, JobRetryPolicy},
    rows::{DayStatus, StreakState, StreakStateRow},
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum FinaliseError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("integer conversion failed")]
    IntegerConversion,
}

pub async fn finalise_due(pool: &PgPool, max_days: u32) -> Result<u32, FinaliseError> {
    let mut finalised = 0_u32;

    for _ in 0..max_days {
        if !finalise_one(pool).await? {
            break;
        }
        finalised += 1;
    }

    Ok(finalised)
}

pub async fn finalise_one(pool: &PgPool) -> Result<bool, FinaliseError> {
    let now = Utc::now();
    let grace = Duration::hours(2);
    let mut tx = pool.begin().await?;

    let Some(candidate) = db_finalise::oldest_finalisable_day(&mut tx, now, grace).await? else {
        tx.commit().await?;
        return Ok(false);
    };

    db_finalise::lock_enrollment(&mut tx, candidate.enrollment_id).await?;

    let Some(day) =
        db_finalise::oldest_unfinalised_day_for_enrollment(&mut tx, candidate.enrollment_id)
            .await?
    else {
        tx.commit().await?;
        return Ok(true);
    };

    let streak_row = db_finalise::streak_state_for_update(&mut tx, day.enrollment_id).await?;
    let rest_declared = rest_declared(&mut tx, day.enrollment_id, day.local_date).await?;

    let score = scoring::day_score(day.earned_points, day.available_points);
    let core_status = scoring::status_from_score(score, rest_declared);
    let outcome = outcome_from_core_status(core_status);
    let before = streak_from_row(&streak_row)?;
    let repair_available = repair_available(&streak_row, day.local_date);
    let finalisation = streak::finalise(&before, outcome, repair_available);

    match finalisation {
        Finalisation::Settled(step) => {
            let stored_status = stored_day_status(core_status, step.rewritten);
            db_finalise::update_day_finalisation(
                &mut tx,
                day.id,
                stored_status,
                day.earned_points,
                now,
            )
            .await?;
            db_finalise::update_streak_state(
                &mut tx,
                new_streak_state(
                    day.enrollment_id,
                    day.local_date,
                    &step.streak,
                    streak_row.repair_used_month,
                )?,
            )
            .await?;
        }
        Finalisation::HeldForRepair { visible, on_expiry } => {
            db_finalise::update_day_finalisation(
                &mut tx,
                day.id,
                DayStatus::Missed,
                day.earned_points,
                now,
            )
            .await?;
            db_finalise::update_streak_state(
                &mut tx,
                new_streak_state(
                    day.enrollment_id,
                    day.local_date,
                    &visible,
                    streak_row.repair_used_month,
                )?,
            )
            .await?;

            let payload = BreakRepairablePayload {
                enrollment_id: day.enrollment_id,
                day_id: day.id,
                current: u32_to_i32(on_expiry.streak.current)?,
                longest: u32_to_i32(on_expiry.streak.longest)?,
                freezes: u8_to_i16(on_expiry.streak.freezes),
                clean_run: u8_to_i16(on_expiry.streak.clean_run),
                state: db_streak_state(on_expiry.streak.state),
                last_counted_date: day.local_date,
            };
            jobs::enqueue_job(
                &mut tx,
                "break_repairable",
                payload,
                day.closes_at + Duration::hours(24),
                JobRetryPolicy::default(),
            )
            .await?;
        }
    }

    tx.commit().await?;
    Ok(true)
}

fn outcome_from_core_status(status: CoreDayStatus) -> DayOutcome {
    match status {
        CoreDayStatus::Complete => DayOutcome::Complete,
        CoreDayStatus::Partial => DayOutcome::Partial,
        CoreDayStatus::Missed => DayOutcome::Missed,
        CoreDayStatus::Rest => DayOutcome::Rest,
    }
}

fn stored_day_status(status: CoreDayStatus, rewritten: Option<DayOutcome>) -> DayStatus {
    if rewritten == Some(DayOutcome::Rest) {
        return DayStatus::Frozen;
    }

    match status {
        CoreDayStatus::Complete => DayStatus::Complete,
        CoreDayStatus::Partial => DayStatus::Partial,
        CoreDayStatus::Missed => DayStatus::Missed,
        CoreDayStatus::Rest => DayStatus::Rest,
    }
}

fn streak_from_row(row: &StreakStateRow) -> Result<Streak, FinaliseError> {
    Ok(Streak {
        current: i32_to_u32(row.current)?,
        longest: i32_to_u32(row.longest)?,
        freezes: i16_to_u8(row.freezes)?,
        clean_run: i16_to_u8(row.clean_run)?,
        state: core_streak_state(row.state.clone()),
    })
}

fn new_streak_state(
    enrollment_id: Uuid,
    local_date: NaiveDate,
    streak: &Streak,
    repair_used_month: Option<NaiveDate>,
) -> Result<db_finalise::NewStreakState, FinaliseError> {
    Ok(db_finalise::NewStreakState {
        enrollment_id,
        current: u32_to_i32(streak.current)?,
        longest: u32_to_i32(streak.longest)?,
        freezes: u8_to_i16(streak.freezes),
        clean_run: u8_to_i16(streak.clean_run),
        last_counted_date: Some(local_date),
        repair_used_month,
        state: db_streak_state(streak.state),
    })
}

fn core_streak_state(state: StreakState) -> streak::StreakState {
    match state {
        StreakState::Active => streak::StreakState::Active,
        StreakState::AtRisk => streak::StreakState::AtRisk,
        StreakState::Repairable => streak::StreakState::Repairable,
        StreakState::Broken => streak::StreakState::Broken,
    }
}

fn db_streak_state(state: streak::StreakState) -> StreakState {
    match state {
        streak::StreakState::Active => StreakState::Active,
        streak::StreakState::AtRisk => StreakState::AtRisk,
        streak::StreakState::Repairable => StreakState::Repairable,
        streak::StreakState::Broken => StreakState::Broken,
    }
}

fn repair_available(streak: &StreakStateRow, local_date: NaiveDate) -> bool {
    streak
        .repair_used_month
        .is_none_or(|used| used.year() != local_date.year() || used.month() != local_date.month())
}

async fn rest_declared(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    enrollment_id: Uuid,
    local_date: NaiveDate,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        select exists (
          select 1
          from rest_days r
          join enrollments e on e.user_id = r.user_id
          where e.id = $1
            and r.local_date = $2
        )
        "#,
    )
    .bind(enrollment_id)
    .bind(local_date)
    .fetch_one(&mut **tx)
    .await
}

fn i32_to_u32(value: i32) -> Result<u32, FinaliseError> {
    u32::try_from(value).map_err(|_| FinaliseError::IntegerConversion)
}

fn u32_to_i32(value: u32) -> Result<i32, FinaliseError> {
    i32::try_from(value).map_err(|_| FinaliseError::IntegerConversion)
}

fn i16_to_u8(value: i16) -> Result<u8, FinaliseError> {
    u8::try_from(value).map_err(|_| FinaliseError::IntegerConversion)
}

fn u8_to_i16(value: u8) -> i16 {
    i16::from(value)
}

#[derive(Clone, Debug, Serialize)]
struct BreakRepairablePayload {
    enrollment_id: Uuid,
    day_id: Uuid,
    current: i32,
    longest: i32,
    freezes: i16,
    clean_run: i16,
    state: StreakState,
    last_counted_date: NaiveDate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_status_wins_when_freeze_rewrites_a_miss() {
        assert_eq!(
            stored_day_status(CoreDayStatus::Missed, Some(DayOutcome::Rest)),
            DayStatus::Frozen
        );
    }

    #[test]
    fn repair_is_available_once_per_calendar_month() {
        let row = StreakStateRow {
            enrollment_id: Uuid::now_v7(),
            current: 0,
            longest: 0,
            freezes: 0,
            clean_run: 0,
            last_counted_date: None,
            repair_used_month: NaiveDate::from_ymd_opt(2026, 7, 1),
            state: StreakState::Active,
        };

        assert!(!repair_available(
            &row,
            NaiveDate::from_ymd_opt(2026, 7, 31).expect("valid")
        ));
        assert!(repair_available(
            &row,
            NaiveDate::from_ymd_opt(2026, 8, 1).expect("valid")
        ));
    }
}

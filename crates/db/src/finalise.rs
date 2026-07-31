use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::{DayRow, DayStatus, StreakState, StreakStateRow};

pub async fn oldest_finalisable_day(
    tx: &mut Transaction<'_, Postgres>,
    now: DateTime<Utc>,
    grace: Duration,
) -> Result<Option<DayRow>, sqlx::Error> {
    sqlx::query_as::<_, DayRow>(
        r#"
        select id, enrollment_id, local_date, day_index, status, available_points,
               earned_points, note, opens_at, closes_at, finalised_at
        from days
        where finalised_at is null
          and closes_at + ($2::bigint * interval '1 second') < $1
        order by closes_at, enrollment_id
        for update skip locked
        limit 1
        "#,
    )
    .bind(now)
    .bind(grace.num_seconds())
    .fetch_optional(&mut **tx)
    .await
}

pub async fn lock_enrollment(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
) -> Result<(), sqlx::Error> {
    let lock_key = advisory_lock_key(enrollment_id);

    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(lock_key)
        .execute(&mut **tx)
        .await?;

    Ok(())
}

pub async fn streak_state_for_update(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
) -> Result<StreakStateRow, sqlx::Error> {
    sqlx::query_as::<_, StreakStateRow>(
        r#"
        select enrollment_id, current, longest, freezes, clean_run,
               last_counted_date, repair_used_month, state
        from streak_states
        where enrollment_id = $1
        for update
        "#,
    )
    .bind(enrollment_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn update_day_finalisation(
    tx: &mut Transaction<'_, Postgres>,
    day_id: Uuid,
    status: DayStatus,
    earned_points: i32,
    finalised_at: DateTime<Utc>,
) -> Result<DayRow, sqlx::Error> {
    sqlx::query_as::<_, DayRow>(
        r#"
        update days
        set status = $2,
            earned_points = $3,
            finalised_at = $4
        where id = $1
          and finalised_at is null
        returning id, enrollment_id, local_date, day_index, status, available_points,
                  earned_points, note, opens_at, closes_at, finalised_at
        "#,
    )
    .bind(day_id)
    .bind(status)
    .bind(earned_points)
    .bind(finalised_at)
    .fetch_one(&mut **tx)
    .await
}

pub async fn update_streak_state(
    tx: &mut Transaction<'_, Postgres>,
    next: NewStreakState,
) -> Result<StreakStateRow, sqlx::Error> {
    sqlx::query_as::<_, StreakStateRow>(
        r#"
        update streak_states
        set current = $2,
            longest = $3,
            freezes = $4,
            clean_run = $5,
            last_counted_date = $6,
            repair_used_month = $7,
            state = $8
        where enrollment_id = $1
        returning enrollment_id, current, longest, freezes, clean_run,
                  last_counted_date, repair_used_month, state
        "#,
    )
    .bind(next.enrollment_id)
    .bind(next.current)
    .bind(next.longest)
    .bind(next.freezes)
    .bind(next.clean_run)
    .bind(next.last_counted_date)
    .bind(next.repair_used_month)
    .bind(next.state)
    .fetch_one(&mut **tx)
    .await
}

pub async fn oldest_unfinalised_day_for_enrollment(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
) -> Result<Option<DayRow>, sqlx::Error> {
    sqlx::query_as::<_, DayRow>(
        r#"
        select id, enrollment_id, local_date, day_index, status, available_points,
               earned_points, note, opens_at, closes_at, finalised_at
        from days
        where enrollment_id = $1
          and finalised_at is null
        order by local_date
        limit 1
        "#,
    )
    .bind(enrollment_id)
    .fetch_optional(&mut **tx)
    .await
}

#[derive(Clone, Debug)]
pub struct NewStreakState {
    pub enrollment_id: Uuid,
    pub current: i32,
    pub longest: i32,
    pub freezes: i16,
    pub clean_run: i16,
    pub last_counted_date: Option<NaiveDate>,
    pub repair_used_month: Option<NaiveDate>,
    pub state: StreakState,
}

fn advisory_lock_key(enrollment_id: Uuid) -> i64 {
    let bytes = enrollment_id.as_bytes();
    let mut high = [0_u8; 8];
    high.copy_from_slice(&bytes[..8]);
    i64::from_be_bytes(high)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advisory_lock_key_is_stable_for_an_enrollment() {
        let enrollment_id =
            Uuid::parse_str("018ff9c0-0000-7000-8000-000000000401").expect("test uuid is valid");

        assert_eq!(advisory_lock_key(enrollment_id), 112583118736617472);
    }
}

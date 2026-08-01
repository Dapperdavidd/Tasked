//! Week buckets: where floating (`n_per_week`) tasks are actually scored.
//!
//! A floating task appears on every day of the ISO week and is excluded from
//! every day's `available_points`, so an untouched one can never mark a day
//! missed. Its points live here instead, on a per-week quota.
//!
//! Without these rows a floating task scores **zero, permanently** — it is
//! excluded from the daily denominator by design and has nothing else to
//! contribute to. "Gym 3x a week" would be invisible in consistency.
//!
//! ## Why `completed` is recounted rather than incremented
//!
//! The obvious implementation is `set completed = completed + 1` on completion.
//! It is wrong here for three reasons, and the failure is silent:
//!
//! - `complete_task` is deliberately idempotent (`coalesce(completed_at, $2)`),
//!   and the offline sync queue replays mutations, so a double tap or a resent
//!   batch would inflate the quota.
//! - Two devices completing two different sessions concurrently race on the
//!   same row.
//! - Any drift is permanent, because nothing ever recomputes it.
//!
//! Recounting from `task_instances` is idempotent by construction, immune to
//! both races, and self-healing: a bucket that somehow drifted is corrected the
//! next time anything in that week is touched. The count is over at most seven
//! days of rows.

use chrono::NaiveDate;
use sqlx::{Postgres, Transaction};
use tracked_core::weekly::{iso_week_of, WeekBucket};
use uuid::Uuid;

use crate::rows::WeekBucketRow;

impl WeekBucketRow {
    /// Lift a stored row into the domain type, so the "is the quota met"
    /// decision is made in exactly one place.
    pub fn to_domain(&self) -> WeekBucket {
        WeekBucket {
            required: self.required.max(0) as u32,
            completed: self.completed.max(0) as u32,
            points_each: self.points_each,
        }
    }
}

/// Create the bucket for a floating template's week if it does not exist yet.
///
/// Called by the materialiser the first time an `n_per_week` template fires in
/// a given ISO week. `on conflict do nothing` matters: materialisation runs
/// twice, and re-running must not reset a week's progress to zero.
pub async fn ensure_bucket(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    template_id: Uuid,
    local_date: NaiveDate,
    required: u32,
    points_each: i32,
) -> Result<WeekBucketRow, sqlx::Error> {
    let week = iso_week_of(local_date);

    sqlx::query_as::<_, WeekBucketRow>(
        r#"
        insert into week_buckets (
          enrollment_id, iso_year, iso_week, template_id,
          required, completed, points_each
        )
        values ($1, $2, $3, $4, $5, 0, $6)
        on conflict (enrollment_id, iso_year, iso_week, template_id)
        do update set
          -- Keep the stored progress. Only the quota and value may be restated,
          -- so an edited template takes effect without wiping the week.
          required = excluded.required,
          points_each = excluded.points_each
        returning enrollment_id, iso_year, iso_week, template_id,
                  required, completed, points_each
        "#,
    )
    .bind(enrollment_id)
    .bind(week.year)
    .bind(week.week as i32)
    .bind(template_id)
    .bind(required as i32)
    .bind(points_each)
    .fetch_all(&mut **tx)
    .await
    .and_then(|mut rows| rows.pop().ok_or_else(|| sqlx::Error::RowNotFound))
}

/// Recount a bucket's completions from the underlying task instances.
///
/// Call after any completion or un-completion of a floating task. Idempotent:
/// calling it ten times produces the same answer as calling it once.
pub async fn refresh_bucket(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    template_id: Uuid,
    local_date: NaiveDate,
) -> Result<Option<WeekBucketRow>, sqlx::Error> {
    let week = iso_week_of(local_date);

    sqlx::query_as::<_, WeekBucketRow>(
        r#"
        update week_buckets wb
        set completed = (
          select count(*)
          from task_instances ti
          join days d on d.id = ti.day_id
          where ti.template_id = wb.template_id
            and ti.is_floating
            and ti.completed_at is not null
            and d.enrollment_id = wb.enrollment_id
            and extract(isoyear from d.local_date)::int = wb.iso_year
            and extract(week    from d.local_date)::int = wb.iso_week
        )
        where wb.enrollment_id = $1
          and wb.template_id = $2
          and wb.iso_year = $3
          and wb.iso_week = $4
        returning wb.enrollment_id, wb.iso_year, wb.iso_week, wb.template_id,
                  wb.required, wb.completed, wb.points_each
        "#,
    )
    .bind(enrollment_id)
    .bind(template_id)
    .bind(week.year)
    .bind(week.week as i32)
    .fetch_optional(&mut **tx)
    .await
}

/// Every bucket for an enrollment in the ISO week containing `local_date`.
pub async fn buckets_for_week(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    local_date: NaiveDate,
) -> Result<Vec<WeekBucketRow>, sqlx::Error> {
    let week = iso_week_of(local_date);

    sqlx::query_as::<_, WeekBucketRow>(
        r#"
        select enrollment_id, iso_year, iso_week, template_id,
               required, completed, points_each
        from week_buckets
        where enrollment_id = $1 and iso_year = $2 and iso_week = $3
        order by template_id
        "#,
    )
    .bind(enrollment_id)
    .bind(week.year)
    .bind(week.week as i32)
    .fetch_all(&mut **tx)
    .await
}

/// Templates whose weekly quota is already met.
///
/// The today view drops these rows for the rest of the week rather than showing
/// them ticked: the point of the quota is that the *week* is done, not that
/// today is.
pub async fn retired_template_ids(
    tx: &mut Transaction<'_, Postgres>,
    enrollment_id: Uuid,
    local_date: NaiveDate,
) -> Result<Vec<Uuid>, sqlx::Error> {
    Ok(buckets_for_week(tx, enrollment_id, local_date)
        .await?
        .into_iter()
        .filter(|row| row.to_domain().quota_met())
        .map(|row| row.template_id)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(required: i32, completed: i32) -> WeekBucketRow {
        WeekBucketRow {
            enrollment_id: Uuid::nil(),
            iso_year: 2026,
            iso_week: 31,
            template_id: Uuid::nil(),
            required,
            completed,
            points_each: 30,
        }
    }

    #[test]
    fn a_stored_row_lifts_into_the_domain_type() {
        assert!(!row(3, 1).to_domain().quota_met());
        assert!(row(3, 3).to_domain().quota_met());
        assert_eq!(row(3, 1).to_domain().earned_points(), 30);
        assert_eq!(row(3, 1).to_domain().available_points(), 90);
    }

    /// Defensive: the columns are plain `int`, so a negative can only arrive
    /// via a bad migration or a support script, and it must not become a huge
    /// unsigned number when it does.
    #[test]
    fn negative_stored_values_do_not_wrap_into_enormous_quotas() {
        let domain = row(-1, -5).to_domain();
        assert_eq!(domain.required, 0);
        assert_eq!(domain.completed, 0);
        assert!(domain.quota_met(), "a zero quota is trivially met");
    }

    /// Over-delivery is clamped in the domain type, so a bucket that somehow
    /// recorded four completions against a quota of three cannot report more
    /// than 100%.
    #[test]
    fn over_delivery_cannot_push_a_bucket_past_its_quota() {
        let domain = row(3, 4).to_domain();
        assert_eq!(domain.earned_points(), domain.available_points());
    }
}

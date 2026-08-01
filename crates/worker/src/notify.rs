use chrono::{DateTime, Duration, NaiveTime, Utc};
use chrono_tz::Tz;
use serde_json::json;
use sqlx::PgPool;
use tracked_core::{
    calendar,
    notify::{self as core_notify, Candidate, Dropped, NotificationKind, QuietHours},
};
use tracked_db::{devices, notifications};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("calendar error: {0:?}")]
    Calendar(calendar::CalendarError),
}

pub async fn enqueue_due(pool: &PgPool, now: DateTime<Utc>) -> Result<u32, NotifyError> {
    let mut tx = pool.begin().await?;
    let users = notifications::notification_users(&mut tx).await?;
    let mut inserted = 0_u32;

    for user in users {
        let Ok(timezone) = user.timezone.parse::<Tz>() else {
            continue;
        };
        let local_date =
            calendar::enrollment_today(now, 0, timezone).map_err(NotifyError::Calendar)?;
        let local_time = now.with_timezone(&timezone).time();
        let remaining =
            notifications::remaining_task_count(&mut tx, user.user_id, local_date).await?;
        let at_risk =
            notifications::at_risk_streak_count(&mut tx, user.user_id, local_date).await?;
        let repairable = notifications::repairable_count(&mut tx, user.user_id).await?;

        let mut drafts = Vec::new();
        if due_now(local_time, user.morning_at, Duration::minutes(15)) {
            drafts.push(EventDraft::morning(now, remaining));
        }
        if due_now(local_time, user.evening_at, Duration::minutes(15)) {
            drafts.push(EventDraft::evening(now, remaining, at_risk));
            if at_risk > 0 {
                drafts.push(EventDraft::at_risk(now, at_risk));
            }
        }
        if repairable > 0 {
            drafts.push(EventDraft::repair(now, repairable));
        }

        if drafts.is_empty() {
            continue;
        }

        let quiet = quiet_hours(user.quiet_start, user.quiet_end);
        let already_sent =
            notifications::sent_event_times(&mut tx, user.user_id, now, Duration::hours(24))
                .await?;
        let candidates = drafts
            .iter()
            .map(|draft| Candidate {
                kind: draft.kind,
                at: draft.at,
            })
            .collect::<Vec<_>>();
        let plan = core_notify::plan(&candidates, quiet, timezone, &already_sent, &[]);

        for candidate in &plan.send {
            if let Some(draft) = drafts.iter().find(|draft| draft.kind == candidate.kind) {
                if notifications::insert_event(
                    &mut tx,
                    notifications::NewNotificationEvent {
                        id: Uuid::now_v7(),
                        user_id: user.user_id,
                        kind: kind_value(draft.kind),
                        scheduled_at: draft.at,
                        title: draft.title,
                        body: &draft.body,
                        payload: draft.payload.clone(),
                        status: "queued",
                        skipped_reason: None,
                    },
                )
                .await?
                .is_some()
                {
                    inserted += 1;
                }
            }
        }

        for (candidate, reason) in plan.dropped {
            if let Some(draft) = drafts.iter().find(|draft| draft.kind == candidate.kind) {
                notifications::insert_event(
                    &mut tx,
                    notifications::NewNotificationEvent {
                        id: Uuid::now_v7(),
                        user_id: user.user_id,
                        kind: kind_value(draft.kind),
                        scheduled_at: draft.at,
                        title: draft.title,
                        body: &draft.body,
                        payload: draft.payload.clone(),
                        status: "skipped",
                        skipped_reason: Some(drop_reason(reason)),
                    },
                )
                .await?;
            }
        }
    }

    tx.commit().await?;
    Ok(inserted)
}

pub async fn deliver_queued(pool: &PgPool, max_events: i64) -> Result<u32, NotifyError> {
    let mut tx = pool.begin().await?;
    let events = notifications::queued_events(&mut tx, max_events).await?;
    let mut delivered = 0_u32;

    for event in events {
        let device_rows = devices::enabled_devices_for_user(&mut tx, event.user_id).await?;
        if device_rows.is_empty() {
            notifications::finish_event(&mut tx, event.id, "failed").await?;
            continue;
        }

        for device in device_rows {
            if let Some(delivery) = notifications::create_delivery(&mut tx, &event, &device).await?
            {
                notifications::mark_delivery_sent(&mut tx, delivery.id).await?;
                delivered += 1;
            }
        }

        notifications::finish_event(&mut tx, event.id, "sent").await?;
    }

    tx.commit().await?;
    Ok(delivered)
}

#[derive(Clone, Debug)]
struct EventDraft {
    kind: NotificationKind,
    at: DateTime<Utc>,
    title: &'static str,
    body: String,
    payload: serde_json::Value,
}

impl EventDraft {
    fn morning(at: DateTime<Utc>, remaining: i64) -> Self {
        let body = if remaining == 1 {
            "1 task waiting today".to_owned()
        } else {
            format!("{remaining} tasks waiting today")
        };
        Self {
            kind: NotificationKind::MorningCard,
            at,
            title: "Today",
            body,
            payload: json!({ "remaining_tasks": remaining }),
        }
    }

    fn evening(at: DateTime<Utc>, remaining: i64, at_risk: i64) -> Self {
        let body = if at_risk > 0 {
            "A streak is at risk. Finish what matters.".to_owned()
        } else if remaining == 0 {
            "Today is logged.".to_owned()
        } else if remaining == 1 {
            "1 task left today".to_owned()
        } else {
            format!("{remaining} tasks left today")
        };
        Self {
            kind: NotificationKind::EveningCheckIn,
            at,
            title: "Check in",
            body,
            payload: json!({ "remaining_tasks": remaining, "at_risk_streaks": at_risk }),
        }
    }

    fn at_risk(at: DateTime<Utc>, count: i64) -> Self {
        Self {
            kind: NotificationKind::StreakAtRisk,
            at,
            title: "Streak at risk",
            body: "Finish enough today to protect it.".to_owned(),
            payload: json!({ "at_risk_streaks": count }),
        }
    }

    fn repair(at: DateTime<Utc>, count: i64) -> Self {
        Self {
            kind: NotificationKind::RepairAvailable,
            at,
            title: "Repair available",
            body: "A missed day can still be repaired today.".to_owned(),
            payload: json!({ "repairable_enrollments": count }),
        }
    }
}

fn due_now(local_time: NaiveTime, scheduled: NaiveTime, window: Duration) -> bool {
    let delta = local_time.signed_duration_since(scheduled);
    delta >= Duration::zero() && delta < window
}

fn quiet_hours(start: Option<NaiveTime>, end: Option<NaiveTime>) -> Option<QuietHours> {
    Some(QuietHours {
        start: start?,
        end: end?,
    })
}

fn kind_value(kind: NotificationKind) -> &'static str {
    match kind {
        NotificationKind::RepairAvailable => "repair_available",
        NotificationKind::StreakAtRisk => "streak_at_risk",
        NotificationKind::MorningCard => "morning_card",
        NotificationKind::EveningCheckIn => "evening_check_in",
        NotificationKind::StandingDrift => "standing_drift",
        NotificationKind::CohortPulse => "cohort_pulse",
    }
}

fn drop_reason(reason: Dropped) -> &'static str {
    match reason {
        Dropped::QuietHours => "quiet_hours",
        Dropped::DailyCapReached => "daily_cap_reached",
        Dropped::ComposedIntoAnother => "composed_into_another",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_window_is_half_open() {
        let scheduled = NaiveTime::from_hms_opt(7, 30, 0).expect("valid");
        assert!(due_now(scheduled, scheduled, Duration::minutes(15)));
        assert!(due_now(
            NaiveTime::from_hms_opt(7, 44, 59).expect("valid"),
            scheduled,
            Duration::minutes(15)
        ));
        assert!(!due_now(
            NaiveTime::from_hms_opt(7, 45, 0).expect("valid"),
            scheduled,
            Duration::minutes(15)
        ));
        assert!(!due_now(
            NaiveTime::from_hms_opt(7, 29, 59).expect("valid"),
            scheduled,
            Duration::minutes(15)
        ));
    }

    #[test]
    fn kind_values_match_database_check_constraint() {
        assert_eq!(kind_value(NotificationKind::MorningCard), "morning_card");
        assert_eq!(
            kind_value(NotificationKind::RepairAvailable),
            "repair_available"
        );
    }
}

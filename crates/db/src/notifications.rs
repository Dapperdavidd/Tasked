use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use crate::rows::{NotificationDeliveryRow, NotificationEventRow};

#[derive(Clone, Debug, FromRow)]
pub struct NotificationUserRow {
    pub user_id: Uuid,
    pub timezone: String,
    pub morning_at: NaiveTime,
    pub evening_at: NaiveTime,
    pub quiet_start: Option<NaiveTime>,
    pub quiet_end: Option<NaiveTime>,
}

#[derive(Clone, Debug)]
pub struct NewNotificationEvent<'a> {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: &'a str,
    pub scheduled_at: DateTime<Utc>,
    pub title: &'a str,
    pub body: &'a str,
    pub payload: Value,
    pub status: &'a str,
    pub skipped_reason: Option<&'a str>,
}

pub async fn notification_users(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<NotificationUserRow>, sqlx::Error> {
    sqlx::query_as::<_, NotificationUserRow>(
        r#"
        select us.user_id, us.timezone, us.morning_at, us.evening_at,
               us.quiet_start, us.quiet_end
        from user_settings us
        where exists (
          select 1 from devices d
          where d.user_id = us.user_id
            and d.enabled
        )
        order by us.user_id
        "#,
    )
    .fetch_all(&mut **tx)
    .await
}

pub async fn remaining_task_count(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    local_date: chrono::NaiveDate,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        select count(*)
        from enrollments e
        join days d on d.enrollment_id = e.id
        join task_instances ti on ti.day_id = d.id
        where e.user_id = $1
          and e.status = 'active'
          and d.local_date = $2
          and ti.completed_at is null
          and ti.skipped_reason is null
        "#,
    )
    .bind(user_id)
    .bind(local_date)
    .fetch_one(&mut **tx)
    .await
}

pub async fn at_risk_streak_count(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    local_date: chrono::NaiveDate,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        select count(*)
        from enrollments e
        join days d on d.enrollment_id = e.id
        join streak_states s on s.enrollment_id = e.id
        where e.user_id = $1
          and e.status = 'active'
          and d.local_date = $2
          and s.current >= 3
          and d.available_points > 0
          and (100 * d.earned_points / nullif(d.available_points, 0)) < 50
        "#,
    )
    .bind(user_id)
    .bind(local_date)
    .fetch_one(&mut **tx)
    .await
}

pub async fn repairable_count(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        select count(*)
        from enrollments e
        join streak_states s on s.enrollment_id = e.id
        where e.user_id = $1
          and e.status = 'active'
          and s.state = 'repairable'
        "#,
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn sent_event_times(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    around: DateTime<Utc>,
    window: Duration,
) -> Result<Vec<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar(
        r#"
        select scheduled_at
        from notification_events
        where user_id = $1
          and status = 'sent'
          and scheduled_at > $2
          and scheduled_at < $3
        order by scheduled_at
        "#,
    )
    .bind(user_id)
    .bind(around - window)
    .bind(around + window)
    .fetch_all(&mut **tx)
    .await
}

pub async fn insert_event(
    tx: &mut Transaction<'_, Postgres>,
    event: NewNotificationEvent<'_>,
) -> Result<Option<NotificationEventRow>, sqlx::Error> {
    sqlx::query_as::<_, NotificationEventRow>(
        r#"
        insert into notification_events (
          id, user_id, kind, scheduled_at, title, body, payload, status, skipped_reason, sent_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9,
                case when $8 = 'sent' then now() else null end)
        on conflict (user_id, kind, scheduled_at) do nothing
        returning id, user_id, kind, scheduled_at, title, body, payload, status,
                  skipped_reason, created_at, sent_at
        "#,
    )
    .bind(event.id)
    .bind(event.user_id)
    .bind(event.kind)
    .bind(event.scheduled_at)
    .bind(event.title)
    .bind(event.body)
    .bind(event.payload)
    .bind(event.status)
    .bind(event.skipped_reason)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn queued_events(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<NotificationEventRow>, sqlx::Error> {
    sqlx::query_as::<_, NotificationEventRow>(
        r#"
        select id, user_id, kind, scheduled_at, title, body, payload, status,
               skipped_reason, created_at, sent_at
        from notification_events
        where status = 'queued'
        order by scheduled_at, created_at
        limit $1
        for update skip locked
        "#,
    )
    .bind(limit)
    .fetch_all(&mut **tx)
    .await
}

pub async fn create_delivery(
    tx: &mut Transaction<'_, Postgres>,
    event: &NotificationEventRow,
    device: &crate::rows::DeviceRow,
) -> Result<Option<NotificationDeliveryRow>, sqlx::Error> {
    sqlx::query_as::<_, NotificationDeliveryRow>(
        r#"
        insert into notification_deliveries (
          id, event_id, user_id, device_id, push_provider, push_token, status
        )
        values ($1, $2, $3, $4, $5, $6, 'queued')
        on conflict (event_id, device_id) do nothing
        returning id, event_id, user_id, device_id, push_provider, push_token, status,
                  attempted_at, delivered_at, last_error, created_at
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(event.id)
    .bind(event.user_id)
    .bind(device.id)
    .bind(device.push_provider.clone())
    .bind(&device.push_token)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn mark_delivery_sent(
    tx: &mut Transaction<'_, Postgres>,
    delivery_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        update notification_deliveries
        set status = 'sent',
            attempted_at = now(),
            delivered_at = now(),
            last_error = null
        where id = $1
        "#,
    )
    .bind(delivery_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn finish_event(
    tx: &mut Transaction<'_, Postgres>,
    event_id: Uuid,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        update notification_events
        set status = $2,
            sent_at = case when $2 = 'sent' then now() else sent_at end
        where id = $1
        "#,
    )
    .bind(event_id)
    .bind(status)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn events_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    limit: i64,
) -> Result<Vec<NotificationEventRow>, sqlx::Error> {
    sqlx::query_as::<_, NotificationEventRow>(
        r#"
        select id, user_id, kind, scheduled_at, title, body, payload, status,
               skipped_reason, created_at, sent_at
        from notification_events
        where user_id = $1
        order by scheduled_at desc, created_at desc
        limit $2
        "#,
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await
}

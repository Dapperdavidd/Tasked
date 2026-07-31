use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::rows::DeviceRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterDevicePlatform {
    Ios,
    Android,
    Web,
}

impl RegisterDevicePlatform {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::Ios => "ios",
            Self::Android => "android",
            Self::Web => "web",
        }
    }
}

pub async fn register_expo_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    push_token: &str,
    platform: Option<RegisterDevicePlatform>,
) -> Result<DeviceRow, sqlx::Error> {
    let id = Uuid::now_v7();
    let platform = platform.map(RegisterDevicePlatform::as_db_value);

    sqlx::query_as::<_, DeviceRow>(
        r#"
        insert into devices (
          id,
          user_id,
          push_provider,
          push_token,
          platform,
          enabled,
          last_seen_at
        )
        values ($1, $2, 'expo', $3, $4, true, now())
        on conflict (push_provider, push_token)
        do update set
          user_id = excluded.user_id,
          platform = excluded.platform,
          enabled = true,
          last_seen_at = now()
        returning id, user_id, push_provider, push_token, platform, enabled, created_at, last_seen_at
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(push_token)
    .bind(platform)
    .fetch_one(&mut **tx)
    .await
}

pub async fn disable_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    device_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        update devices
        set enabled = false
        where id = $1
          and user_id = $2
        "#,
    )
    .bind(device_id)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub async fn enabled_devices_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<DeviceRow>, sqlx::Error> {
    sqlx::query_as::<_, DeviceRow>(
        r#"
        select id, user_id, push_provider, push_token, platform, enabled, created_at, last_seen_at
        from devices
        where user_id = $1
          and enabled
        order by last_seen_at desc nulls last, created_at desc
        "#,
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await
}

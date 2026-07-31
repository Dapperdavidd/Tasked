use actix_web::{delete, post, web, HttpResponse};
use serde::{Deserialize, Serialize};
use tracked_db::{
    devices::{self as devices_db, RegisterDevicePlatform},
    rls,
};
use uuid::Uuid;

use crate::{app::ApiState, auth::UserId, error::ApiError};

#[derive(Deserialize)]
pub struct RegisterDeviceBody {
    push_token: String,
    platform: Option<DevicePlatformBody>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DevicePlatformBody {
    Ios,
    Android,
    Web,
}

#[post("/v1/devices")]
pub async fn register_device(
    state: web::Data<ApiState>,
    user_id: UserId,
    body: web::Json<RegisterDeviceBody>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    let row = devices_db::register_expo_device(
        &mut tx,
        user_id.0,
        &body.push_token,
        body.platform.as_ref().map(platform_to_db),
    )
    .await?;
    tx.commit().await?;
    Ok(HttpResponse::Ok().json(DeviceResponse::from(row)))
}

#[delete("/v1/devices/{id}")]
pub async fn disable_device(
    state: web::Data<ApiState>,
    user_id: UserId,
    path: web::Path<Uuid>,
) -> Result<HttpResponse, ApiError> {
    let mut tx = state.pool.begin().await?;
    rls::set_request_user(&mut tx, user_id.0).await?;
    devices_db::disable_device(&mut tx, user_id.0, *path).await?;
    tx.commit().await?;
    Ok(HttpResponse::NoContent().finish())
}

fn platform_to_db(platform: &DevicePlatformBody) -> RegisterDevicePlatform {
    match platform {
        DevicePlatformBody::Ios => RegisterDevicePlatform::Ios,
        DevicePlatformBody::Android => RegisterDevicePlatform::Android,
        DevicePlatformBody::Web => RegisterDevicePlatform::Web,
    }
}

#[derive(Serialize)]
struct DeviceResponse {
    id: Uuid,
    enabled: bool,
}

impl From<tracked_db::rows::DeviceRow> for DeviceResponse {
    fn from(row: tracked_db::rows::DeviceRow) -> Self {
        Self {
            id: row.id,
            enabled: row.enabled,
        }
    }
}

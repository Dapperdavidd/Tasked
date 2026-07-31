use actix_web::{dev::Payload, FromRequest, HttpRequest};
use std::future::{ready, Ready};
use uuid::Uuid;

use crate::error::ApiError;

#[derive(Clone, Copy, Debug)]
pub struct UserId(pub Uuid);

impl FromRequest for UserId {
    type Error = ApiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let Some(value) = req.headers().get("x-user-id") else {
            return ready(Err(ApiError::MissingUser));
        };
        let Ok(value) = value.to_str() else {
            return ready(Err(ApiError::InvalidUser));
        };
        let Ok(user_id) = Uuid::parse_str(value) else {
            return ready(Err(ApiError::InvalidUser));
        };
        ready(Ok(Self(user_id)))
    }
}

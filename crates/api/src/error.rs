use actix_web::{http::StatusCode, HttpResponse, ResponseError};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("missing X-User-Id header")]
    MissingUser,
    #[error("invalid X-User-Id header")]
    InvalidUser,
    #[error("worker error: {0}")]
    Worker(String),
    #[error("database error")]
    Db(#[from] sqlx::Error),
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::MissingUser | Self::InvalidUser => StatusCode::UNAUTHORIZED,
            Self::Worker(_) | Self::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        if matches!(self, Self::Worker(_) | Self::Db(_)) {
            eprintln!("{self:?}");
        }
        HttpResponse::build(self.status_code()).json(ErrorBody {
            error: self.to_string(),
        })
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

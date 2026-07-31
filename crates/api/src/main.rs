#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

use std::env;

use actix_web::{web, App, HttpServer};
use tracked_api::{app::ApiState, routes};
use tracked_db::pool::{connect, DatabaseConfig};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let database_url = env::var("DATABASE_URL").map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "DATABASE_URL is required")
    })?;
    let bind = env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let pool = connect(&DatabaseConfig::from_url(database_url))
        .await
        .map_err(std::io::Error::other)?;
    let state = ApiState { pool };

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routes::configure)
    })
    .bind(bind)?
    .run()
    .await
}

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

use std::{env, process::ExitCode};

use tracked_db::pool::{connect, DatabaseConfig};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let command = env::args().nth(1).unwrap_or_else(|| "run-once".to_owned());
    let database_url =
        env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required".to_owned())?;
    let pool = connect(&DatabaseConfig::from_url(database_url))
        .await
        .map_err(|error| format!("database connection failed: {error}"))?;

    match command.as_str() {
        "materialise" => {
            let count = tracked_worker::materialise::materialise_due(&pool, chrono::Utc::now())
                .await
                .map_err(|error| format!("materialise failed: {error}"))?;
            println!("materialised {count} day(s)");
        }
        "finalise" => {
            let count = tracked_worker::finalise::finalise_due(&pool, 500)
                .await
                .map_err(|error| format!("finalise failed: {error}"))?;
            println!("finalised {count} day(s)");
        }
        "run-once" => {
            let materialised =
                tracked_worker::materialise::materialise_due(&pool, chrono::Utc::now())
                    .await
                    .map_err(|error| format!("materialise failed: {error}"))?;
            let finalised = tracked_worker::finalise::finalise_due(&pool, 500)
                .await
                .map_err(|error| format!("finalise failed: {error}"))?;
            println!("materialised {materialised} day(s), finalised {finalised} day(s)");
        }
        _ => {
            return Err("usage: tracked-worker [materialise|finalise|run-once]".to_owned());
        }
    }

    Ok(())
}

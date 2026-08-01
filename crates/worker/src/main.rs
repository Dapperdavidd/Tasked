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
            let expired = tracked_worker::finalise::expire_repairable_due(&pool, 500)
                .await
                .map_err(|error| format!("repair expiry failed: {error}"))?;
            println!("finalised {count} day(s), expired {expired} repair window(s)");
        }
        "ingest" => {
            let count = tracked_worker::ingest::process_due(&pool, 100)
                .await
                .map_err(|error| format!("ingest failed: {error}"))?;
            println!("processed {count} ingest job(s)");
        }
        "notify" => {
            let enqueued = tracked_worker::notify::enqueue_due(&pool, chrono::Utc::now())
                .await
                .map_err(|error| format!("notification enqueue failed: {error}"))?;
            let delivered = tracked_worker::notify::deliver_queued(&pool, 500)
                .await
                .map_err(|error| format!("notification delivery failed: {error}"))?;
            println!("enqueued {enqueued} notification event(s), delivered {delivered} device notification(s)");
        }
        "run-once" => {
            let materialised =
                tracked_worker::materialise::materialise_due(&pool, chrono::Utc::now())
                    .await
                    .map_err(|error| format!("materialise failed: {error}"))?;
            let finalised = tracked_worker::finalise::finalise_due(&pool, 500)
                .await
                .map_err(|error| format!("finalise failed: {error}"))?;
            let expired = tracked_worker::finalise::expire_repairable_due(&pool, 500)
                .await
                .map_err(|error| format!("repair expiry failed: {error}"))?;
            let ingested = tracked_worker::ingest::process_due(&pool, 100)
                .await
                .map_err(|error| format!("ingest failed: {error}"))?;
            let enqueued = tracked_worker::notify::enqueue_due(&pool, chrono::Utc::now())
                .await
                .map_err(|error| format!("notification enqueue failed: {error}"))?;
            let delivered = tracked_worker::notify::deliver_queued(&pool, 500)
                .await
                .map_err(|error| format!("notification delivery failed: {error}"))?;
            println!(
                "materialised {materialised} day(s), finalised {finalised} day(s), expired {expired} repair window(s), processed {ingested} ingest job(s), enqueued {enqueued} notification event(s), delivered {delivered} device notification(s)"
            );
        }
        _ => {
            return Err(
                "usage: tracked-worker [materialise|finalise|ingest|notify|run-once]".to_owned(),
            );
        }
    }

    Ok(())
}

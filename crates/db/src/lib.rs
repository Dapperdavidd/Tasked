#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

pub mod artifacts;
pub mod cohorts;
pub mod devices;
pub mod finalise;
pub mod idempotency;
pub mod jobs;
pub mod materialise;
pub mod pool;
pub mod rest_days;
pub mod rls;
pub mod rows;
pub mod standing;
pub mod stats;
pub mod tasks;
pub mod today;

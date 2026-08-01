#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

//! The ingestion pipeline: the thing that makes this product worth switching to.
//!
//! Five stages, each a separate resumable step:
//!
//! 1. **normalise** — route by MIME type to text plus a content hash.
//! 2. **classify** — cheap model call: curriculum, routine, or project.
//! 3. **generate** — the expensive call, validated against a schema derived
//!    from [`types::GeneratedProgram`] before it is deserialised.
//! 4. **calibrate** — [`calibrate`], deterministic Rust, no model.
//! 5. **ready** — persist the draft and notify the client.
//!
//! Only stage 4 is implemented here so far; it is the one the system design
//! insists must be code rather than a prompt, and the one that decides whether
//! a user quits on day three.
//!
//! Ingestion is for **bounded programs only**. Standing tasks are created
//! manually from three cadence presets and never touch this pipeline.

pub mod calibrate;
pub mod types;

pub use calibrate::{calibrate, project, Calibration};
pub use types::{GeneratedProgram, GeneratedTask, Intensity, ProgramKind, Warning};

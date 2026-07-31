---
project: Tracked
document: System Design
version: 0.2
date: 2026-07-31
owner: Dapper
companion: tracked-prd.md
stack: Rust with Actix Web, Postgres, TypeScript clients
scope: backend, data, pipeline. Frontend implementation deferred.
---

# Tracked system design

This repo is implementing Tracked in phases from the supplied system design.

Phase 1 covers the foundation:

- Rust workspace.
- Pure `crates/core` domain logic.
- Postgres migrations.
- Tests for scoring, cadence, calendar, and streak behavior.

Core invariants:

- Days are materialised, not computed on read.
- Enrollments are the unit for scores, streaks, heatmaps, finalisation, and stats.
- The job queue lives in Postgres.
- Ingestion is asynchronous and idempotent.
- `crates/core` contains no I/O, no database access, no HTTP, and no system clock.

Implementation order:

1. Workspace skeleton, migrations, `crates/core` scoring/cadence/calendar.
2. Streak fold and property tests.
3. `crates/db`, materialiser, finaliser jobs.
4. Today endpoint and completion loop.
5. Standing enrollment and standing CRUD.
6. Ingestion.
7. Notifications.
8. Stats, heatmap, drift.
9. Cohorts.
10. Completion artifact.

The full user-supplied design text should be treated as authoritative for decisions not yet copied into implementation notes.

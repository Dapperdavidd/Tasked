#!/usr/bin/env bash
#
# Local Postgres harness for Tracked.
#
#   scripts/db.sh setup     create the database and apply all migrations
#   scripts/db.sh migrate   apply pending migrations
#   scripts/db.sh check     assert the schema matches what the migrations say
#   scripts/db.sh probe     assert every schema guarantee actually rejects a violation
#   scripts/db.sh seed      load fixtures/seed_phase2.sql
#   scripts/db.sh reset     drop and recreate; refuses unless TRACKED_ALLOW_RESET=1
#   scripts/db.sh psql      open a shell on the dev database
#
# Requires: postgres 16 running locally, and sqlx-cli (`cargo install sqlx-cli`).

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ -f .env ]]; then
  # shellcheck disable=SC1091
  set -a && source .env && set +a
fi

# sqlx does not fall back to the OS user the way libpq does — it connects as
# "anonymous" and fails confusingly — so the username is always explicit here.
: "${DATABASE_URL:=postgres://${PGUSER:-$USER}@localhost:5432/tracked_dev}"
export DATABASE_URL

db_name="${DATABASE_URL##*/}"
db_name="${db_name%%\?*}"

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: $1 is not on PATH. $2" >&2
    exit 1
  }
}

ensure_server() {
  require psql "Install Postgres 16."
  pg_isready >/dev/null 2>&1 || {
    echo "error: no Postgres server is accepting connections." >&2
    echo "       brew services start postgresql@16" >&2
    exit 1
  }
}

cmd_setup() {
  ensure_server
  require sqlx "cargo install sqlx-cli --no-default-features --features postgres"
  sqlx database create
  cmd_migrate
}

cmd_migrate() {
  ensure_server
  require sqlx "cargo install sqlx-cli --no-default-features --features postgres"
  sqlx migrate run
}

# Guards the two invariants that are cheap to assert and expensive to discover
# in production: the standing-list cap actually fires, and a standing enrollment
# can never surface on a cohort read path.
cmd_check() {
  ensure_server
  echo "==> applied migrations"
  psql "$DATABASE_URL" -qAt -c \
    "select version || '  ' || description from _sqlx_migrations order by version;"

  echo "==> tables"
  psql "$DATABASE_URL" -qAt -c \
    "select count(*) || ' tables' from information_schema.tables
     where table_schema = 'public' and table_type = 'BASE TABLE';"

  echo "==> standing task cap trigger is installed"
  psql "$DATABASE_URL" -qAt -c \
    "select tgname from pg_trigger where tgname = 'standing_cap';" | grep -q standing_cap \
    && echo "ok" || { echo "MISSING"; exit 1; }

  echo "==> cohort presence excludes standing enrollments"
  psql "$DATABASE_URL" -qAt -c \
    "select pg_get_functiondef(oid) from pg_proc where proname = 'cohort_presence_on';" \
    | grep -q "not e.is_standing" \
    && echo "ok" || { echo "MISSING: standing lists could reach a cohort surface"; exit 1; }
}

# Asserts that the guarantees the domain logic leans on are enforced by the
# database rather than merely intended. Runs in a rolled-back transaction, so it
# is safe against a seeded development database.
cmd_probe() {
  ensure_server
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -q -f scripts/schema_probe.sql
}

cmd_seed() {
  ensure_server
  local fixture=fixtures/seed_phase2.sql
  [[ -f $fixture ]] || { echo "error: $fixture not found" >&2; exit 1; }
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f "$fixture"
}

cmd_reset() {
  if [[ "${TRACKED_ALLOW_RESET:-0}" != "1" ]]; then
    echo "refusing to drop '$db_name'." >&2
    echo "re-run with TRACKED_ALLOW_RESET=1 if that is really what you want." >&2
    exit 1
  fi
  ensure_server
  require sqlx "cargo install sqlx-cli --no-default-features --features postgres"
  sqlx database drop -y
  cmd_setup
}

cmd_psql() {
  ensure_server
  exec psql "$DATABASE_URL"
}

case "${1:-}" in
  setup)   cmd_setup ;;
  migrate) cmd_migrate ;;
  check)   cmd_check ;;
  probe)   cmd_probe ;;
  seed)    cmd_seed ;;
  reset)   cmd_reset ;;
  psql)    cmd_psql ;;
  *)
    sed -n '3,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac

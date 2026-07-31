-- Schema probes: assert that the guarantees the domain logic leans on are
-- actually enforced by the database, not just intended by it.
--
-- Each probe is a statement that MUST be rejected. A probe that succeeds is the
-- bug. Everything runs inside a transaction that is rolled back, so this is safe
-- against a seeded development database.
--
-- Run with: scripts/db.sh probe
--
-- Two design rules, both learned the hard way:
--
--   1. A rejection only counts if it came from an integrity constraint or an
--      explicit RAISE. Any other SQLSTATE means the probe statement itself is
--      broken — a typo'd column rejects beautifully and proves nothing — so it
--      is reported as a failure rather than swallowed.
--   2. Nothing that records a failure may sit inside an exception handler's
--      scope. The first version of this file appended to a text[] with an
--      untyped literal, which raised 22P02, which the handler caught, which
--      meant every probe passed unconditionally.
--
-- Fixture IDs are fixed rather than generated so the probe statements can refer
-- to them literally. The transaction is rolled back, so they never persist.

\set ON_ERROR_STOP on

begin;

do $probe$
declare
  failures text[] := array[]::text[];
  accepted boolean;
  probe    record;
  leaked   int;

  u  constant uuid := '11111111-1111-4111-8111-000000000001';  -- user
  pb constant uuid := '11111111-1111-4111-8111-000000000002';  -- bounded program
  ps constant uuid := '11111111-1111-4111-8111-000000000003';  -- standing program
  eb constant uuid := '11111111-1111-4111-8111-000000000004';  -- bounded enrollment
  es constant uuid := '11111111-1111-4111-8111-000000000005';  -- standing enrollment
  dy constant uuid := '11111111-1111-4111-8111-000000000006';  -- a day
  tt constant uuid := '11111111-1111-4111-8111-000000000007';  -- a task template
  ch constant uuid := '11111111-1111-4111-8111-000000000008';  -- a cohort
begin
  -- ---- fixtures ------------------------------------------------------------
  insert into users (id, email) values (u, 'probe@example.invalid');

  insert into programs (id, title, kind, duration_days)
    values (pb, 'probe bounded', 'curriculum', 30);
  insert into programs (id, title, kind)
    values (ps, 'probe standing', 'standing');

  insert into task_templates
    (id, program_id, position, title, difficulty, estimated_minutes, cadence, points)
  values
    (tt, pb, 0, 'probe task', 1, 10, '{"type":"daily"}'::jsonb, 10);

  insert into enrollments (id, user_id, program_id, timezone, start_date, is_standing)
    values (eb, u, pb, 'Africa/Lagos', '2026-08-01', false),
           (es, u, ps, 'Africa/Lagos', '2026-08-01', true);

  insert into days (id, enrollment_id, local_date, day_index, opens_at, closes_at)
    values (dy, eb, '2026-08-01', 0,
            '2026-07-31T23:00:00Z', '2026-08-01T23:00:00Z');

  insert into task_instances (id, day_id, template_id, title, points, position)
    values (gen_random_uuid(), dy, tt, 'probe task', 10, 0);

  -- Fill the standing list exactly to its cap of five.
  insert into task_templates
    (id, program_id, position, title, difficulty, estimated_minutes, cadence, points)
  select gen_random_uuid(), ps, n, 'standing filler', 1, 10,
         '{"type":"daily"}'::jsonb, 10
  from generate_series(1, 5) n;

  -- ---- probes --------------------------------------------------------------
  for probe in
    select * from (values
      -- An unrecognised cadence must be refused at write time. Discovering it
      -- in the materialiser means a user's day silently has no tasks in it.
      ('an unknown cadence type',
       format($s$insert into task_templates
                   (id, program_id, position, title, difficulty,
                    estimated_minutes, cadence, points)
                 values (gen_random_uuid(), %L, 90, 'x', 1, 10,
                         '{"type":"every_other_tuesday"}'::jsonb, 10)$s$, pb)),

      -- Standing programs have no finish line: no duration, no source.
      ('a standing program with a duration',
       $s$insert into programs (id, title, kind, duration_days)
          values (gen_random_uuid(), 'x', 'standing', 30)$s$),

      -- Bounded programs must be bounded.
      ('a bounded program with no duration',
       $s$insert into programs (id, title, kind)
          values (gen_random_uuid(), 'x', 'curriculum')$s$),

      -- Exactly one standing enrollment per user.
      ('a second standing enrollment for one user',
       format($s$insert into enrollments
                   (id, user_id, program_id, timezone, start_date, is_standing)
                 values (gen_random_uuid(), %L, %L, 'Africa/Lagos',
                         '2026-08-01', true)$s$, u, ps)),

      -- The day boundary is 00:00 to 04:00 local, per PRD F2.
      ('a day boundary hour above 4',
       format($s$update enrollments set day_boundary_hour = 5 where id = %L$s$, eb)),

      -- Materialisation runs twice. It must be idempotent at the day level...
      ('the same enrollment day materialised twice',
       format($s$insert into days
                   (id, enrollment_id, local_date, day_index, opens_at, closes_at)
                 values (gen_random_uuid(), %L, '2026-08-01', 0,
                         '2026-07-31T23:00:00Z', '2026-08-01T23:00:00Z')$s$, eb)),

      -- ...and at the instance level.
      ('the same template instantiated twice on one day',
       format($s$insert into task_instances
                   (id, day_id, template_id, title, points, position)
                 values (gen_random_uuid(), %L, %L, 'probe task', 10, 1)$s$, dy, tt)),

      -- The standing list cap is the feature. Five, and no upgrade path.
      ('a sixth standing task',
       format($s$insert into task_templates
                   (id, program_id, position, title, difficulty,
                    estimated_minutes, cadence, points)
                 values (gen_random_uuid(), %L, 6, 'sixth', 1, 10,
                         '{"type":"daily"}'::jsonb, 10)$s$, ps)),

      ('a difficulty outside 1..5',
       format($s$insert into task_templates
                   (id, program_id, position, title, difficulty,
                    estimated_minutes, cadence, points)
                 values (gen_random_uuid(), %L, 91, 'x', 9, 10,
                         '{"type":"daily"}'::jsonb, 10)$s$, pb)),

      ('an estimated duration outside 1..480',
       format($s$insert into task_templates
                   (id, program_id, position, title, difficulty,
                    estimated_minutes, cadence, points)
                 values (gen_random_uuid(), %L, 92, 'x', 1, 9000,
                         '{"type":"daily"}'::jsonb, 10)$s$, pb)),

      ('an unknown day status',
       format($s$update days set status = 'nearly' where id = %L$s$, dy)),

      ('a fourth banked freeze',
       format($s$insert into streak_states (enrollment_id, freezes)
                 values (%L, 4)$s$, eb))
    ) as t(label, stmt)
  loop
    accepted := false;

    begin
      execute probe.stmt;
      accepted := true;
    exception
      -- Rejected as intended: a constraint, or the standing-cap trigger's RAISE.
      when integrity_constraint_violation or raise_exception then
        accepted := false;
      -- Anything else means the probe statement is broken, not that the schema
      -- is sound. Report it loudly instead of scoring it as a pass.
      when others then
        failures := array_append(
          failures,
          format('probe for %s is broken: %s %s', probe.label, SQLSTATE, SQLERRM)
        );
        accepted := false;
    end;

    if accepted then
      failures := array_append(failures, format('%s was accepted', probe.label));
    end if;
  end loop;

  -- ---- cohort isolation ----------------------------------------------------
  -- A standing list must never be reachable from a cohort surface, even when
  -- the enrollment row itself carries a cohort_id.
  insert into cohorts (id, program_id, owner_id) values (ch, ps, u);
  update enrollments set cohort_id = ch where id = es;
  insert into streak_states (enrollment_id, current) values (es, 9);

  select count(*) into leaked from cohort_presence_on(ch, '2026-08-01'::date);

  if leaked <> 0 then
    failures := array_append(
      failures,
      format('%s standing enrollment(s) surfaced through cohort_presence_on', leaked)
    );
  end if;

  -- ---- verdict -------------------------------------------------------------
  if array_length(failures, 1) > 0 then
    raise exception E'schema probe failed:\n  - %', array_to_string(failures, E'\n  - ');
  end if;

  raise notice 'all schema probes passed';
end
$probe$;

rollback;

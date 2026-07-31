-- Schema probes: assert that the guarantees the domain logic leans on are
-- actually enforced by the database, not just intended by it.
--
-- Every probe attempts a write that MUST fail. A probe that succeeds is the
-- bug. Run inside a transaction that is rolled back, so this is safe against a
-- seeded development database.
--
-- Run with: scripts/db.sh probe

\set ON_ERROR_STOP on

begin;

do $probe$
declare
  failures  text[] := '{}';
  v_user    uuid := gen_random_uuid();
  v_bounded uuid := gen_random_uuid();
  v_standing uuid := gen_random_uuid();
  v_enrol   uuid := gen_random_uuid();
  v_standing_enrol uuid := gen_random_uuid();
  v_day     uuid := gen_random_uuid();
  v_template uuid := gen_random_uuid();
  v_cohort  uuid := gen_random_uuid();
  v_leaked  int;
begin
  -- ---- fixtures, rolled back at the end -----------------------------------
  insert into users (id, email) values (v_user, 'probe@example.invalid');

  insert into programs (id, title, kind, duration_days)
    values (v_bounded, 'probe bounded', 'curriculum', 30);
  insert into programs (id, title, kind)
    values (v_standing, 'probe standing', 'standing');

  insert into task_templates
    (id, program_id, position, title, difficulty, estimated_minutes, cadence, points)
  values
    (v_template, v_bounded, 0, 'probe task', 1, 10, '{"type":"daily"}'::jsonb, 10);

  insert into enrollments (id, user_id, program_id, timezone, start_date, is_standing)
    values (v_enrol, v_user, v_bounded, 'Africa/Lagos', '2026-08-01', false),
           (v_standing_enrol, v_user, v_standing, 'Africa/Lagos', '2026-08-01', true);

  insert into days (id, enrollment_id, local_date, day_index, opens_at, closes_at)
    values (v_day, v_enrol, '2026-08-01', 0,
            '2026-07-31T23:00:00Z', '2026-08-01T23:00:00Z');

  -- ---- probes --------------------------------------------------------------

  -- An unrecognised cadence must be refused at write time. Discovering it in
  -- the materialiser means a user's day silently has no tasks in it.
  begin
    insert into task_templates
      (id, program_id, position, title, difficulty, estimated_minutes, cadence, points)
    values
      (gen_random_uuid(), v_bounded, 1, 'x', 1, 10,
       '{"type":"every_other_tuesday"}'::jsonb, 10);
    failures := failures || 'unknown cadence type was accepted';
  exception when others then null;
  end;

  -- Standing programs have no finish line, so no duration and no source.
  begin
    insert into programs (id, title, kind, duration_days)
      values (gen_random_uuid(), 'x', 'standing', 30);
    failures := failures || 'standing program accepted a duration';
  exception when others then null;
  end;

  -- Bounded programs must be bounded.
  begin
    insert into programs (id, title, kind) values (gen_random_uuid(), 'x', 'curriculum');
    failures := failures || 'bounded program accepted a null duration';
  exception when others then null;
  end;

  -- Exactly one standing enrollment per user.
  begin
    insert into enrollments (id, user_id, program_id, timezone, start_date, is_standing)
      values (gen_random_uuid(), v_user, v_standing, 'Africa/Lagos', '2026-08-01', true);
    failures := failures || 'a user was given a second standing enrollment';
  exception when others then null;
  end;

  -- The day boundary is 00:00 to 04:00 local, per PRD F2.
  begin
    update enrollments set day_boundary_hour = 5 where id = v_enrol;
    failures := failures || 'day_boundary_hour above 4 was accepted';
  exception when others then null;
  end;

  -- Materialisation runs twice. It must be idempotent at the day level.
  begin
    insert into days (id, enrollment_id, local_date, day_index, opens_at, closes_at)
      values (gen_random_uuid(), v_enrol, '2026-08-01', 0,
              '2026-07-31T23:00:00Z', '2026-08-01T23:00:00Z');
    failures := failures || 'the same enrollment day was materialised twice';
  exception when others then null;
  end;

  -- ...and at the instance level.
  insert into task_instances (id, day_id, template_id, title, points, position)
    values (gen_random_uuid(), v_day, v_template, 'probe task', 10, 0);
  begin
    insert into task_instances (id, day_id, template_id, title, points, position)
      values (gen_random_uuid(), v_day, v_template, 'probe task', 10, 0);
    failures := failures || 'the same template was instantiated twice on one day';
  exception when others then null;
  end;

  -- The standing list cap is the feature. Five, and no upgrade path.
  insert into task_templates
    (id, program_id, position, title, difficulty, estimated_minutes, cadence, points)
  select gen_random_uuid(), v_standing, n, 'standing filler', 1, 10,
         '{"type":"daily"}'::jsonb, 10
  from generate_series(1, 5) n;
  begin
    insert into task_templates
      (id, program_id, position, title, difficulty, estimated_minutes, cadence, points)
    values
      (gen_random_uuid(), v_standing, 6, 'sixth', 1, 10, '{"type":"daily"}'::jsonb, 10);
    failures := failures || 'a sixth standing task was accepted';
  exception when others then null;
  end;

  begin
    insert into task_templates
      (id, program_id, position, title, difficulty, estimated_minutes, cadence, points)
    values
      (gen_random_uuid(), v_bounded, 2, 'x', 9, 10, '{"type":"daily"}'::jsonb, 10);
    failures := failures || 'difficulty outside 1..5 was accepted';
  exception when others then null;
  end;

  -- A standing list must never be reachable from a cohort surface, even when
  -- the enrollment row itself carries a cohort_id.
  insert into cohorts (id, program_id, owner_id) values (v_cohort, v_standing, v_user);
  update enrollments set cohort_id = v_cohort where id = v_standing_enrol;
  insert into streak_states (enrollment_id, current) values (v_standing_enrol, 9);

  select count(*) into v_leaked
  from cohort_presence_on(v_cohort, '2026-08-01'::date);

  if v_leaked <> 0 then
    failures := failures || 'a standing enrollment surfaced through cohort_presence_on';
  end if;

  -- ---- verdict -------------------------------------------------------------
  if array_length(failures, 1) > 0 then
    raise exception 'schema probe failed: %', array_to_string(failures, '; ');
  end if;

  raise notice 'all schema probes passed';
end
$probe$;

rollback;

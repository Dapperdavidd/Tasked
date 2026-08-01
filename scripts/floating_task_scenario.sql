-- Scenario: a floating ("gym 3x a week") task across one ISO week.
--
-- Unit tests cannot reach the logic that matters here, because it lives in SQL.
-- This walks a real week against a real database and asserts the behaviour the
-- system design specifies in section 4.5.
--
-- The two helpers below are deliberate copies of the statements shipped in
-- `crates/db/src/tasks.rs` and `crates/db/src/week_buckets.rs`. If those change
-- and this is not updated, the copies diverge and this stops proving anything —
-- so keep them in step.
--
-- The assertion that earns its keep is idempotency. Task completion is
-- deliberately idempotent at the instance level and the offline queue replays
-- mutations, so a naive `completed = completed + 1` would inflate the quota on
-- a double tap and nothing would ever correct it.
--
-- Runs in a transaction that is rolled back.
--
-- Run with: scripts/db.sh scenario

\set ON_ERROR_STOP on

begin;

-- Mirrors the idempotent completion in crates/db/src/tasks.rs.
create function pg_temp.complete_floating(p_enrollment uuid, p_template uuid, p_date date)
returns void language sql as $$
  update task_instances ti
  set completed_at = coalesce(ti.completed_at, now())
  from days d
  where d.id = ti.day_id
    and d.enrollment_id = p_enrollment
    and d.local_date = p_date
    and ti.template_id = p_template;
$$;

-- Mirrors refresh_bucket in crates/db/src/week_buckets.rs.
create function pg_temp.refresh_bucket(p_enrollment uuid, p_template uuid, p_year int, p_week int)
returns week_buckets language sql as $$
  update week_buckets wb
  set completed = (
    select count(*)
    from task_instances ti
    join days d on d.id = ti.day_id
    where ti.template_id = wb.template_id
      and ti.is_floating
      and ti.completed_at is not null
      and d.enrollment_id = wb.enrollment_id
      and extract(isoyear from d.local_date)::int = wb.iso_year
      and extract(week    from d.local_date)::int = wb.iso_week
  )
  where wb.enrollment_id = p_enrollment
    and wb.template_id = p_template
    and wb.iso_year = p_year
    and wb.iso_week = p_week
  returning wb.*;
$$;

do $scenario$
declare
  failures text[] := array[]::text[];

  u  constant uuid := '22222222-2222-4222-8222-000000000001';
  p  constant uuid := '22222222-2222-4222-8222-000000000002';
  e  constant uuid := '22222222-2222-4222-8222-000000000003';
  t  constant uuid := '22222222-2222-4222-8222-000000000004';
  ft constant uuid := '22222222-2222-4222-8222-000000000005';

  -- 2026-07-27 is a Monday, so the whole week is ISO 2026-W31.
  week_start constant date := '2026-07-27';

  day_id      uuid;
  offset_days int;
  bucket      week_buckets%rowtype;
  denominators text;
begin
  -- ---- fixtures ------------------------------------------------------------
  insert into users (id, email) values (u, 'floating@example.invalid');
  insert into programs (id, title, kind, duration_days)
    values (p, 'probe routine', 'routine', 30);

  insert into task_templates
    (id, program_id, position, title, difficulty, estimated_minutes, cadence, points)
  values
    (t,  p, 0, 'gym',     3, 45, '{"type":"n_per_week","count":3}'::jsonb, 30),
    (ft, p, 1, 'stretch', 1, 10, '{"type":"daily"}'::jsonb, 10);

  insert into enrollments (id, user_id, program_id, timezone, start_date, is_standing)
    values (e, u, p, 'Africa/Lagos', week_start, false);

  for offset_days in 0..6 loop
    day_id := gen_random_uuid();
    insert into days (id, enrollment_id, local_date, day_index, opens_at, closes_at)
      values (
        day_id, e, week_start + offset_days, offset_days,
        (week_start + offset_days)::timestamptz,
        (week_start + offset_days + 1)::timestamptz
      );

    insert into task_instances (id, day_id, template_id, title, points, position, is_floating)
      values
        (gen_random_uuid(), day_id, t,  'gym',     30, 0, true),
        (gen_random_uuid(), day_id, ft, 'stretch', 10, 1, false);
  end loop;

  -- ---- creating the bucket twice must not reset progress -------------------
  insert into week_buckets
    (enrollment_id, iso_year, iso_week, template_id, required, completed, points_each)
  values (e, 2026, 31, t, 3, 0, 30)
  on conflict (enrollment_id, iso_year, iso_week, template_id)
  do update set required = excluded.required, points_each = excluded.points_each;

  perform pg_temp.complete_floating(e, t, week_start);
  bucket := pg_temp.refresh_bucket(e, t, 2026, 31);

  -- Materialisation runs twice. Re-running it here must leave the one recorded
  -- session alone.
  insert into week_buckets
    (enrollment_id, iso_year, iso_week, template_id, required, completed, points_each)
  values (e, 2026, 31, t, 3, 0, 30)
  on conflict (enrollment_id, iso_year, iso_week, template_id)
  do update set required = excluded.required, points_each = excluded.points_each;

  select * into bucket from week_buckets
   where enrollment_id = e and template_id = t and iso_year = 2026 and iso_week = 31;

  if bucket.completed <> 1 then
    failures := array_append(failures,
      format('re-materialising reset the week to %s', bucket.completed));
  end if;

  -- ---- Wednesday too -------------------------------------------------------
  perform pg_temp.complete_floating(e, t, week_start + 2);
  bucket := pg_temp.refresh_bucket(e, t, 2026, 31);

  if bucket.completed <> 2 then
    failures := array_append(failures,
      format('after two sessions the bucket read %s', bucket.completed));
  end if;

  -- ---- the assertion that matters ------------------------------------------
  perform pg_temp.complete_floating(e, t, week_start);
  bucket := pg_temp.refresh_bucket(e, t, 2026, 31);

  if bucket.completed <> 2 then
    failures := array_append(failures,
      format('a repeated completion inflated the quota to %s', bucket.completed));
  end if;

  -- ---- Friday meets the quota ----------------------------------------------
  perform pg_temp.complete_floating(e, t, week_start + 4);
  bucket := pg_temp.refresh_bucket(e, t, 2026, 31);

  if bucket.completed <> 3 then
    failures := array_append(failures,
      format('after three sessions the bucket read %s', bucket.completed));
  end if;

  -- ---- un-ticking Wednesday frees the slot again ---------------------------
  update task_instances ti
  set completed_at = null
  from days d
  where d.id = ti.day_id and d.enrollment_id = e
    and ti.template_id = t and d.local_date = week_start + 2;

  bucket := pg_temp.refresh_bucket(e, t, 2026, 31);
  if bucket.completed <> 2 then
    failures := array_append(failures,
      format('un-ticking left the bucket at %s', bucket.completed));
  end if;

  -- ---- a non-floating instance of the same template is not counted ---------
  update task_instances ti
  set is_floating = false, completed_at = now()
  from days d
  where d.id = ti.day_id and d.enrollment_id = e
    and ti.template_id = t and d.local_date = week_start + 5;

  bucket := pg_temp.refresh_bucket(e, t, 2026, 31);
  if bucket.completed <> 2 then
    failures := array_append(failures,
      format('a non-floating instance was counted, bucket read %s', bucket.completed));
  end if;

  -- ---- work stayed inside its own ISO week ---------------------------------
  if exists (
    select 1 from week_buckets
    where enrollment_id = e and template_id = t and (iso_year, iso_week) <> (2026, 31)
  ) then
    failures := array_append(failures, 'week 31 work leaked into another week');
  end if;

  -- ---- floating points never enter a day denominator -----------------------
  -- The whole reason buckets exist: an untouched floating task must never be
  -- able to mark a day missed.
  select string_agg(distinct total::text, ',' order by total::text)
    into denominators
  from (
    select coalesce(sum(ti.points) filter (where not ti.is_floating), 0) as total
    from days d
    join task_instances ti on ti.day_id = d.id
    where d.enrollment_id = e
    group by d.id
  ) per_day;

  -- Every day is worth the one 10-point fixed task, except the day whose
  -- floating instance the is_floating probe above flipped to fixed.
  if denominators is distinct from '10,40' then
    failures := array_append(failures,
      format('unexpected daily denominators: %s', denominators));
  end if;

  -- ---- verdict -------------------------------------------------------------
  if array_length(failures, 1) > 0 then
    raise exception E'floating task scenario failed:\n  - %',
      array_to_string(failures, E'\n  - ');
  end if;

  raise notice 'floating task scenario passed';
end
$scenario$;

rollback;

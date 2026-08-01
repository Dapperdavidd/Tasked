-- Development fixture for phase 2+ materialiser/finaliser work.
-- It creates templates and enrollments only. Days/task_instances should be produced by jobs.

insert into users (id, email, display_name, avatar_url)
values (
  '018ff9c0-0000-7000-8000-000000000001',
  'dapper@example.com',
  'Dapper',
  null
)
on conflict (email) do nothing;

insert into user_settings (
  user_id,
  timezone,
  day_boundary_hour,
  morning_at,
  evening_at,
  locale
)
values (
  '018ff9c0-0000-7000-8000-000000000001',
  'Africa/Lagos',
  0,
  '07:30',
  '20:30',
  'en'
)
on conflict (user_id) do nothing;

insert into programs (
  id,
  author_id,
  title,
  summary,
  kind,
  duration_days,
  intensity,
  source_id
)
values (
  '018ff9c0-0000-7000-8000-000000000101',
  '018ff9c0-0000-7000-8000-000000000001',
  '8-Week 5K Plan',
  'A standard-intensity beginner 5K training block.',
  'routine',
  56,
  'standard',
  null
)
on conflict (id) do nothing;

insert into programs (
  id,
  author_id,
  title,
  summary,
  kind,
  duration_days,
  intensity,
  source_id
)
values (
  '018ff9c0-0000-7000-8000-000000000102',
  '018ff9c0-0000-7000-8000-000000000001',
  'Standing List',
  'Private capped baseline tasks.',
  'standing',
  null,
  null,
  null
)
on conflict (id) do nothing;

insert into task_templates (
  id,
  program_id,
  position,
  title,
  description,
  category,
  difficulty,
  estimated_minutes,
  cadence,
  points
)
values
  (
    '018ff9c0-0000-7000-8000-000000000201',
    '018ff9c0-0000-7000-8000-000000000101',
    1,
    'Run easy for 30 minutes',
    null,
    'Fitness',
    3,
    30,
    '{"type":"weekly_days","days":[1,3,5]}'::jsonb,
    25
  ),
  (
    '018ff9c0-0000-7000-8000-000000000202',
    '018ff9c0-0000-7000-8000-000000000101',
    2,
    'Complete strength training',
    null,
    'Fitness',
    3,
    45,
    '{"type":"weekly_days","days":[2,4]}'::jsonb,
    31
  ),
  (
    '018ff9c0-0000-7000-8000-000000000203',
    '018ff9c0-0000-7000-8000-000000000101',
    3,
    'Do mobility routine',
    null,
    'Fitness',
    2,
    15,
    '{"type":"daily"}'::jsonb,
    11
  ),
  (
    '018ff9c0-0000-7000-8000-000000000204',
    '018ff9c0-0000-7000-8000-000000000101',
    4,
    'Read one running lesson',
    null,
    'Learning',
    1,
    20,
    '{"type":"daily"}'::jsonb,
    8
  ),
  (
    '018ff9c0-0000-7000-8000-000000000205',
    '018ff9c0-0000-7000-8000-000000000101',
    5,
    'Log food and water',
    null,
    'Recovery',
    1,
    5,
    '{"type":"daily"}'::jsonb,
    6
  )
on conflict (id) do nothing;

insert into enrollments (
  id,
  user_id,
  program_id,
  timezone,
  day_boundary_hour,
  start_date,
  is_standing,
  status
)
values
  (
    '018ff9c0-0000-7000-8000-000000000401',
    '018ff9c0-0000-7000-8000-000000000001',
    '018ff9c0-0000-7000-8000-000000000101',
    'Africa/Lagos',
    0,
    '2026-05-24',
    false,
    'active'
  ),
  (
    '018ff9c0-0000-7000-8000-000000000402',
    '018ff9c0-0000-7000-8000-000000000001',
    '018ff9c0-0000-7000-8000-000000000102',
    'Africa/Lagos',
    0,
    '2026-05-24',
    true,
    'active'
  )
on conflict (id) do nothing;

insert into streak_states (enrollment_id)
values
  ('018ff9c0-0000-7000-8000-000000000401'),
  ('018ff9c0-0000-7000-8000-000000000402')
on conflict (enrollment_id) do nothing;

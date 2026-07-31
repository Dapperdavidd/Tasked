create extension if not exists citext;

create table users (
  id            uuid primary key,
  email         citext unique not null,
  display_name  text,
  avatar_url    text,
  created_at    timestamptz not null default now()
);

create table user_settings (
  user_id            uuid primary key references users on delete cascade,
  timezone           text     not null,
  day_boundary_hour  smallint not null default 0
                     check (day_boundary_hour between 0 and 4),
  morning_at         time     not null default '07:30',
  evening_at         time     not null default '20:30',
  quiet_start        time,
  quiet_end          time,
  allow_multi_active boolean  not null default false,
  locale             text     not null default 'en'
);

create table source_documents (
  id             uuid primary key,
  user_id        uuid references users on delete cascade,
  content_hash   bytea not null,
  mime_type      text,
  storage_key    text,
  extracted_text text,
  created_at     timestamptz not null default now()
);

create index source_documents_content_hash_idx on source_documents (content_hash);

create table programs (
  id             uuid primary key,
  author_id      uuid references users on delete set null,
  title          text not null,
  summary        text,
  kind           text not null
                 check (kind in ('curriculum','routine','project','standing')),
  duration_days  int
                 check (duration_days is null or duration_days between 1 and 730),
  intensity      text check (intensity in ('light','standard','heavy')),
  source_id      uuid references source_documents,
  share_titles   boolean not null default false,
  created_at     timestamptz not null default now(),

  constraint duration_matches_kind check (
    (kind = 'standing' and duration_days is null and source_id is null)
    or (kind <> 'standing' and duration_days is not null)
  )
);

create table task_templates (
  id                uuid primary key,
  program_id        uuid not null references programs on delete cascade,
  position          int  not null,
  title             text not null,
  description       text,
  category          text,
  difficulty        smallint not null check (difficulty between 1 and 5),
  estimated_minutes int  not null check (estimated_minutes between 1 and 480),
  cadence           jsonb not null,
  points            int  not null,
  paused_at         timestamptz,
  created_at        timestamptz not null default now(),

  constraint cadence_has_known_type check (
    cadence ? 'type'
    and cadence->>'type' in ('daily', 'weekly_days', 'n_per_week', 'once')
  )
);

create index task_templates_active_program_idx on task_templates (program_id) where paused_at is null;

create or replace function enforce_standing_cap() returns trigger as $$
begin
  if (select kind from programs where id = new.program_id) = 'standing'
     and (select count(*) from task_templates
          where program_id = new.program_id and paused_at is null) >= 5 then
    raise exception 'standing program task cap reached';
  end if;
  return new;
end $$ language plpgsql;

create trigger standing_cap before insert on task_templates
  for each row execute function enforce_standing_cap();

create table cohorts (
  id           uuid primary key,
  program_id   uuid not null references programs,
  owner_id     uuid not null references users,
  name         text,
  locked_start date,
  member_cap   smallint not null default 12,
  created_at   timestamptz not null default now()
);

create table enrollments (
  id                    uuid primary key,
  user_id               uuid not null references users on delete cascade,
  program_id            uuid not null references programs,
  cohort_id             uuid references cohorts,
  timezone              text not null,
  day_boundary_hour     smallint not null default 0 check (day_boundary_hour between 0 and 4),
  start_date            date not null,
  is_standing           boolean not null default false,
  status                text not null default 'active'
                        check (status in ('active','paused','completed','abandoned')),
  materialised_through  date,
  created_at            timestamptz not null default now()
);

create unique index one_standing_per_user
  on enrollments (user_id) where is_standing;

create index enrollments_user_status_idx on enrollments (user_id, status);
create index enrollments_timezone_idx on enrollments (timezone);

create table days (
  id                uuid primary key,
  enrollment_id     uuid not null references enrollments on delete cascade,
  local_date        date not null,
  day_index         int  not null,
  status            text not null default 'open'
                    check (status in ('open','complete','partial','missed','rest','frozen')),
  available_points  int  not null default 0,
  earned_points     int  not null default 0,
  note              text,
  opens_at          timestamptz not null,
  closes_at         timestamptz not null,
  finalised_at      timestamptz,
  unique (enrollment_id, local_date)
);

create index days_enrollment_local_date_desc_idx on days (enrollment_id, local_date desc);
create index days_unfinalised_closes_at_idx on days (closes_at) where finalised_at is null;

create table task_instances (
  id             uuid primary key,
  day_id         uuid not null references days on delete cascade,
  template_id    uuid not null references task_templates,
  title          text not null,
  points         int  not null,
  position       int  not null,
  is_floating    boolean not null default false,
  completed_at   timestamptz,
  skipped_reason text,
  proof_kind     text check (proof_kind in ('note','photo','link')),
  proof_value    text
);

create index task_instances_day_idx on task_instances (day_id);
create index task_instances_day_incomplete_idx on task_instances (day_id) where completed_at is null;

create table streak_states (
  enrollment_id      uuid primary key references enrollments on delete cascade,
  current            int      not null default 0,
  longest            int      not null default 0,
  freezes            smallint not null default 0 check (freezes between 0 and 3),
  clean_run          smallint not null default 0,
  last_counted_date  date,
  repair_used_month  date,
  state              text     not null default 'active'
                     check (state in ('active','at_risk','repairable','broken'))
);

create table week_buckets (
  enrollment_id uuid not null references enrollments on delete cascade,
  iso_year      int  not null,
  iso_week      int  not null,
  template_id   uuid not null references task_templates,
  required      int  not null,
  completed     int  not null default 0,
  points_each   int  not null,
  primary key (enrollment_id, iso_year, iso_week, template_id)
);

create table cohort_invites (
  token      text primary key,
  cohort_id  uuid not null references cohorts on delete cascade,
  expires_at timestamptz,
  max_uses   int,
  uses       int not null default 0
);

create table ingestion_jobs (
  id          uuid primary key,
  source_id   uuid references source_documents,
  user_id     uuid references users on delete cascade,
  instruction text,
  intensity   text not null check (intensity in ('light','standard','heavy')),
  status      text not null default 'queued'
              check (status in ('queued','normalising','classifying',
                                'generating','calibrating','ready','failed')),
  draft       jsonb,
  warnings    jsonb,
  error_code  text,
  created_at  timestamptz not null default now()
);

create table jobs (
  id           uuid primary key,
  kind         text        not null,
  payload      jsonb       not null,
  run_at       timestamptz not null default now(),
  attempts     int         not null default 0,
  max_attempts int         not null default 3,
  locked_until timestamptz,
  failed_at    timestamptz,
  last_error   text
);

create index jobs_run_at_available_idx on jobs (run_at)
  where failed_at is null;

create view cohort_presence as
select e.cohort_id,
       e.user_id,
       u.display_name,
       u.avatar_url,
       s.current as streak,
       d.local_date,
       (d.status in ('complete','partial')) as logged_today
from enrollments e
join users u on u.id = e.user_id
join streak_states s on s.enrollment_id = e.id
left join days d on d.enrollment_id = e.id
where not e.is_standing
  and e.cohort_id is not null;

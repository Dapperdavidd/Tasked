create table idempotency_keys (
  user_id       uuid not null references users on delete cascade,
  key           text not null,
  method        text not null,
  path          text not null,
  request_hash  bytea not null,
  status_code   int not null,
  response_body jsonb not null,
  created_at    timestamptz not null default now(),
  expires_at    timestamptz not null,
  primary key (user_id, key)
);

create index idempotency_keys_expires_at_idx on idempotency_keys (expires_at);

create table rest_days (
  id          uuid primary key,
  user_id     uuid not null references users on delete cascade,
  local_date  date not null,
  reason      text,
  created_at  timestamptz not null default now(),
  unique (user_id, local_date)
);

create index rest_days_user_local_date_desc_idx on rest_days (user_id, local_date desc);

create table devices (
  id             uuid primary key,
  user_id        uuid not null references users on delete cascade,
  push_provider  text not null check (push_provider in ('expo')),
  push_token     text not null,
  platform       text check (platform in ('ios','android','web')),
  enabled        boolean not null default true,
  created_at     timestamptz not null default now(),
  last_seen_at   timestamptz,
  unique (push_provider, push_token)
);

create index devices_user_enabled_idx on devices (user_id) where enabled;

create table completion_artifacts (
  id              uuid primary key,
  enrollment_id   uuid not null references enrollments on delete cascade,
  image_key       text,
  pdf_key         text,
  payload         jsonb not null,
  created_at      timestamptz not null default now(),
  unique (enrollment_id)
);

alter table idempotency_keys enable row level security;
alter table rest_days enable row level security;
alter table devices enable row level security;
alter table completion_artifacts enable row level security;

create policy idempotency_keys_owner_policy on idempotency_keys
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

create policy rest_days_owner_policy on rest_days
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

create policy devices_owner_policy on devices
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

create policy completion_artifacts_owner_policy on completion_artifacts
  using (
    exists (
      select 1
      from enrollments e
      where e.id = completion_artifacts.enrollment_id
        and e.user_id = app_current_user_id()
        and not e.is_standing
    )
  )
  with check (
    exists (
      select 1
      from enrollments e
      where e.id = completion_artifacts.enrollment_id
        and e.user_id = app_current_user_id()
        and not e.is_standing
    )
  );

create table notification_events (
  id              uuid primary key,
  user_id         uuid not null references users on delete cascade,
  kind            text not null
                  check (kind in ('repair_available','streak_at_risk','morning_card',
                                  'evening_check_in','standing_drift','cohort_pulse')),
  scheduled_at    timestamptz not null,
  title           text not null,
  body            text not null,
  payload         jsonb not null default '{}'::jsonb,
  status          text not null default 'queued'
                  check (status in ('queued','sent','skipped','failed')),
  skipped_reason  text,
  created_at      timestamptz not null default now(),
  sent_at         timestamptz,
  unique (user_id, kind, scheduled_at)
);

create index notification_events_user_status_idx
  on notification_events (user_id, status, scheduled_at desc);

create table notification_deliveries (
  id             uuid primary key,
  event_id       uuid not null references notification_events on delete cascade,
  user_id        uuid not null references users on delete cascade,
  device_id      uuid not null references devices on delete cascade,
  push_provider  text not null check (push_provider in ('expo')),
  push_token     text not null,
  status         text not null default 'queued'
                 check (status in ('queued','sent','failed')),
  attempted_at   timestamptz,
  delivered_at   timestamptz,
  last_error     text,
  created_at     timestamptz not null default now(),
  unique (event_id, device_id)
);

create index notification_deliveries_event_idx on notification_deliveries (event_id);
create index notification_deliveries_user_status_idx
  on notification_deliveries (user_id, status, created_at desc);

alter table notification_events enable row level security;
alter table notification_deliveries enable row level security;

create policy notification_events_owner_policy on notification_events
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

create policy notification_deliveries_owner_policy on notification_deliveries
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

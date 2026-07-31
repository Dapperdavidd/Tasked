create function app_current_user_id()
returns uuid
language sql
stable
as $$
  select nullif(current_setting('app.user_id', true), '')::uuid;
$$;

alter table user_settings enable row level security;
alter table source_documents enable row level security;
alter table programs enable row level security;
alter table task_templates enable row level security;
alter table enrollments enable row level security;
alter table days enable row level security;
alter table task_instances enable row level security;
alter table streak_states enable row level security;
alter table week_buckets enable row level security;
alter table cohorts enable row level security;
alter table cohort_invites enable row level security;
alter table ingestion_jobs enable row level security;

create policy user_settings_owner_policy on user_settings
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

create policy source_documents_owner_policy on source_documents
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

create policy programs_author_policy on programs
  using (
    author_id = app_current_user_id()
    or exists (
      select 1
      from enrollments e
      where e.program_id = programs.id
        and e.user_id = app_current_user_id()
    )
  )
  with check (author_id = app_current_user_id());

create policy task_templates_program_access_policy on task_templates
  using (
    exists (
      select 1
      from programs p
      where p.id = task_templates.program_id
        and (
          p.author_id = app_current_user_id()
          or exists (
            select 1
            from enrollments e
            where e.program_id = p.id
              and e.user_id = app_current_user_id()
          )
        )
    )
  )
  with check (
    exists (
      select 1
      from programs p
      where p.id = task_templates.program_id
        and p.author_id = app_current_user_id()
    )
  );

create policy enrollments_owner_policy on enrollments
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

create policy days_owner_policy on days
  using (
    exists (
      select 1
      from enrollments e
      where e.id = days.enrollment_id
        and e.user_id = app_current_user_id()
    )
  )
  with check (
    exists (
      select 1
      from enrollments e
      where e.id = days.enrollment_id
        and e.user_id = app_current_user_id()
    )
  );

create policy task_instances_owner_policy on task_instances
  using (
    exists (
      select 1
      from days d
      join enrollments e on e.id = d.enrollment_id
      where d.id = task_instances.day_id
        and e.user_id = app_current_user_id()
    )
  )
  with check (
    exists (
      select 1
      from days d
      join enrollments e on e.id = d.enrollment_id
      where d.id = task_instances.day_id
        and e.user_id = app_current_user_id()
    )
  );

create policy streak_states_owner_policy on streak_states
  using (
    exists (
      select 1
      from enrollments e
      where e.id = streak_states.enrollment_id
        and e.user_id = app_current_user_id()
    )
  )
  with check (
    exists (
      select 1
      from enrollments e
      where e.id = streak_states.enrollment_id
        and e.user_id = app_current_user_id()
    )
  );

create policy week_buckets_owner_policy on week_buckets
  using (
    exists (
      select 1
      from enrollments e
      where e.id = week_buckets.enrollment_id
        and e.user_id = app_current_user_id()
    )
  )
  with check (
    exists (
      select 1
      from enrollments e
      where e.id = week_buckets.enrollment_id
        and e.user_id = app_current_user_id()
    )
  );

create policy cohorts_owner_policy on cohorts
  using (
    owner_id = app_current_user_id()
    or exists (
      select 1
      from enrollments e
      where e.cohort_id = cohorts.id
        and e.user_id = app_current_user_id()
        and not e.is_standing
    )
  )
  with check (owner_id = app_current_user_id());

create policy cohort_invites_owner_policy on cohort_invites
  using (
    exists (
      select 1
      from cohorts c
      where c.id = cohort_invites.cohort_id
        and c.owner_id = app_current_user_id()
    )
  )
  with check (
    exists (
      select 1
      from cohorts c
      where c.id = cohort_invites.cohort_id
        and c.owner_id = app_current_user_id()
    )
  );

create policy ingestion_jobs_owner_policy on ingestion_jobs
  using (user_id = app_current_user_id())
  with check (user_id = app_current_user_id());

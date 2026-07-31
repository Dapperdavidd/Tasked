drop view if exists cohort_presence;

create function cohort_presence_on(p_cohort_id uuid, p_local_date date)
returns table (
  cohort_id uuid,
  user_id uuid,
  display_name text,
  avatar_url text,
  streak int,
  logged_today boolean
)
language sql
stable
as $$
  select e.cohort_id,
         e.user_id,
         u.display_name,
         u.avatar_url,
         s.current as streak,
         coalesce(d.status in ('complete','partial'), false) as logged_today
  from enrollments e
  join users u on u.id = e.user_id
  join streak_states s on s.enrollment_id = e.id
  left join days d
    on d.enrollment_id = e.id
   and d.local_date = p_local_date
  where e.cohort_id = p_cohort_id
    and not e.is_standing;
$$;

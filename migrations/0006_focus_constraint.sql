-- PRD F4, the focus constraint: one active bounded enrollment per user, plus
-- the standing list, unless the user has opted into multiple.
--
-- The system design notes that the obvious partial unique index
--
--   create unique index on enrollments (user_id)
--     where status = 'active' and not is_standing;
--
-- cannot express the opt-out, because `allow_multi_active` lives on a different
-- table and an index predicate cannot reach it. It concluded the rule therefore
-- has to be enforced in the API.
--
-- This adds a database backstop rather than replacing that check, for the same
-- reason the standing five-task cap has one: a rule that exists only in
-- application code will eventually be violated by a migration script, a support
-- tool, or a second write path that forgot. A trigger can read
-- `user_settings`, so it can honour the opt-out that an index cannot.
--
-- The advisory lock matters. Without it this is a check-then-act, and two
-- concurrent enrollment creations for the same user would both see zero
-- existing rows and both succeed. Locking on the user id serialises the
-- decision per user without touching anyone else's writes.

create or replace function enforce_single_active_bounded() returns trigger as $$
declare
  multi_allowed boolean;
  already_active int;
begin
  -- The standing list never counts against the focus constraint: one program
  -- plus a baseline is not divided attention.
  if new.is_standing or new.status <> 'active' then
    return new;
  end if;

  perform pg_advisory_xact_lock(hashtext(new.user_id::text));

  select coalesce(us.allow_multi_active, false)
    into multi_allowed
  from users u
  left join user_settings us on us.user_id = u.id
  where u.id = new.user_id;

  if coalesce(multi_allowed, false) then
    return new;
  end if;

  select count(*)
    into already_active
  from enrollments e
  where e.user_id = new.user_id
    and e.id <> new.id
    and not e.is_standing
    and e.status = 'active';

  if already_active > 0 then
    raise exception 'user already has an active bounded enrollment'
      using errcode = 'check_violation';
  end if;

  return new;
end $$ language plpgsql;

create trigger single_active_bounded
  before insert or update on enrollments
  for each row execute function enforce_single_active_bounded();

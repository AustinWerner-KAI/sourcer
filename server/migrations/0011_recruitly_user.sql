-- Each team member's own Recruitly user, looked up by email the first time
-- it is needed and kept, so the screen can tell "owned by you" from
-- "owned by a colleague" without asking Recruitly on every page.
ALTER TABLE app_user ADD COLUMN recruitly_user_id text;

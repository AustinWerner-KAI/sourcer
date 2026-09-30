-- Sprint 2: roles, job specs and the brief (SRS F2, F3, F4, F8).

-- The job spec as pasted or uploaded. Sent to the AI provider only to draft
-- the brief; never contains candidate data.
ALTER TABLE role ADD COLUMN spec_text text;

-- Clients are matched by web domain as well as name, so their staff can be
-- kept out of their own roles. Tidy any existing domains to the bare host,
-- clear ones that are not domains, and keep one client per domain (the
-- oldest) before making domains unique.
UPDATE client SET domain = regexp_replace(
    regexp_replace(lower(trim(domain)), '^(https?://)?(www\.)?', ''),
    '[/:?#].*$', '')
WHERE domain IS NOT NULL;
UPDATE client SET domain = NULL
WHERE domain IS NOT NULL AND domain !~ '^[a-z0-9-]+(\.[a-z0-9-]+)+$';
UPDATE client c SET domain = NULL
WHERE domain IS NOT NULL AND EXISTS (
    SELECT 1 FROM client o
    WHERE o.org_id = c.org_id AND o.domain = c.domain
      AND (o.created_at, o.id) < (c.created_at, c.id));
CREATE UNIQUE INDEX client_domain ON client (org_id, lower(domain)) WHERE domain IS NOT NULL;

-- Employer domains, where the data provider gives them, for the same check.
ALTER TABLE person ADD COLUMN current_employer_domain text;
ALTER TABLE employment ADD COLUMN employer_domain text;
-- Saved with no known current employer: a person checks before any contact.
ALTER TABLE candidacy ADD COLUMN employer_unknown boolean NOT NULL DEFAULT false;

-- Level becomes a list of included seniority titles ("Senior", "Lead", ...).
ALTER TABLE brief ALTER COLUMN level TYPE jsonb
    USING CASE WHEN trim(level) = '' THEN '[]'::jsonb ELSE jsonb_build_array(level) END;
ALTER TABLE brief RENAME COLUMN level TO levels;
-- Functional and soft skills, used to rank and explain matches, never to
-- filter. Domains are {name, weight}: "must" narrows the search, "plus" only
-- lifts the ranking.
ALTER TABLE brief ADD COLUMN capabilities jsonb NOT NULL DEFAULT '[]';
ALTER TABLE brief ADD COLUMN domains jsonb NOT NULL DEFAULT '[]';
-- Titles never searched ("Director", "VP", ...).
ALTER TABLE brief ADD COLUMN excluded_titles jsonb NOT NULL DEFAULT '[]';
ALTER TABLE brief ADD COLUMN remote boolean NOT NULL DEFAULT false;
-- Extra companies the resourcer leaves out. The hiring client and off-limits
-- clients are always left out on top of these, in code, and cannot be removed.
ALTER TABLE brief ADD COLUMN leave_out jsonb NOT NULL DEFAULT '[]';
ALTER TABLE brief ADD COLUMN drafted_by_ai boolean NOT NULL DEFAULT false;
ALTER TABLE brief ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();

-- A role has at most one unconfirmed draft. Extra older drafts (none are
-- expected) are removed if nothing uses them. If one is in use, stop here
-- rather than guess: an admin must look at it.
DELETE FROM brief b
WHERE confirmed_at IS NULL
  AND EXISTS (SELECT 1 FROM brief n
              WHERE n.role_id = b.role_id AND n.confirmed_at IS NULL AND n.version > b.version)
  AND NOT EXISTS (SELECT 1 FROM candidacy c WHERE c.brief_id = b.id)
  AND NOT EXISTS (SELECT 1 FROM run r WHERE r.brief_id = b.id);
DO $$
BEGIN
  IF EXISTS (SELECT role_id FROM brief WHERE confirmed_at IS NULL GROUP BY role_id HAVING count(*) > 1) THEN
    RAISE EXCEPTION 'a role has more than one draft brief in use; resolve it before upgrading';
  END IF;
END $$;
CREATE UNIQUE INDEX brief_one_draft ON brief (role_id) WHERE confirmed_at IS NULL;

-- A confirmed brief never changes, whatever the code does. Edits start a new
-- version.
CREATE FUNCTION brief_confirmed_is_final() RETURNS trigger AS $$
BEGIN
  IF OLD.confirmed_at IS NOT NULL THEN
    RAISE EXCEPTION 'brief % is confirmed and cannot change', OLD.id;
  END IF;
  RETURN NEW;
END $$ LANGUAGE plpgsql;
CREATE TRIGGER brief_confirmed_is_final BEFORE UPDATE ON brief
    FOR EACH ROW EXECUTE FUNCTION brief_confirmed_is_final();

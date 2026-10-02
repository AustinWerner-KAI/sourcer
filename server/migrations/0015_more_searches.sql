-- More searches for a role (Kai, 2 Oct 2026): beside the brief, up to three
-- wider searches. Claude chooses two (slots 1 and 2); slot 3 is the
-- resourcer's own. Each search only widens the confirmed brief it belongs to,
-- so the people found still fit the job, and everyone is ranked against the
-- brief. People already found for the role are never paid for again.

CREATE TABLE extra_search (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  role_id     uuid NOT NULL REFERENCES role(id),
  -- The confirmed brief this widens. Confirming the brief again starts afresh.
  brief_id    uuid NOT NULL REFERENCES brief(id),
  slot        smallint NOT NULL CHECK (slot BETWEEN 1 AND 3),
  by_claude   boolean NOT NULL,
  name        text NOT NULL,
  note        text NOT NULL DEFAULT '',
  -- What it widens: titles, levels and places added, tools made nice to
  -- have, and so on. The search is the brief with these applied.
  widen       jsonb NOT NULL,
  -- Goes up each time the widening changes; a count of an older one is stale.
  version     int NOT NULL DEFAULT 1,
  created_by  uuid REFERENCES app_user(id),
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now(),
  UNIQUE (brief_id, slot)
);
CREATE INDEX extra_search_role ON extra_search (role_id);

-- NULL means the brief's own search.
ALTER TABLE search_count ADD COLUMN search_id uuid REFERENCES extra_search(id);
ALTER TABLE search_count ADD COLUMN search_version int;
ALTER TABLE pull ADD COLUMN search_id uuid REFERENCES extra_search(id);
-- The search that first found the person for this role.
ALTER TABLE candidacy ADD COLUMN search_id uuid REFERENCES extra_search(id);

CREATE INDEX search_count_extra ON search_count (search_id, created_at DESC) WHERE search_id IS NOT NULL;

-- M2 outreach, part one: drafts and approval (Kai, 1 Oct 2026).
--
-- One email sequence per candidacy: a first email and two follow-ups, sent
-- only to a personal email address. One approval covers all three. Nothing in
-- this migration sends anything.

-- How each team member signs and introduces themselves in outreach.
ALTER TABLE app_user ADD COLUMN signature text NOT NULL DEFAULT '';
ALTER TABLE app_user ADD COLUMN intro text NOT NULL DEFAULT '';

CREATE TYPE outreach_status AS ENUM ('draft', 'approved', 'active', 'stopped', 'done');

CREATE TABLE outreach (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id        uuid NOT NULL REFERENCES org(id),
  candidacy_id  uuid NOT NULL UNIQUE REFERENCES candidacy(id) ON DELETE CASCADE,
  -- Who it is sent from, and to which address (always a personal email).
  sender_id     uuid NOT NULL REFERENCES app_user(id),
  to_email      text NOT NULL,
  status        outreach_status NOT NULL DEFAULT 'draft',
  -- Why it stopped, in words, e.g. "Replied" or "Stopped by Kai".
  stop_reason   text,
  approved_by   uuid REFERENCES app_user(id),
  approved_at   timestamptz,
  version       int NOT NULL DEFAULT 0,
  created_at    timestamptz NOT NULL DEFAULT now(),
  updated_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE outreach_step (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id        uuid NOT NULL REFERENCES org(id),
  outreach_id   uuid NOT NULL REFERENCES outreach(id) ON DELETE CASCADE,
  step          int NOT NULL CHECK (step BETWEEN 1 AND 3),
  -- Days after the previous email was sent (0 for the first).
  delay_days    int NOT NULL CHECK (delay_days BETWEEN 0 AND 30),
  subject       text NOT NULL,
  body          text NOT NULL,
  sent_at       timestamptz,
  message_id    text,
  UNIQUE (outreach_id, step)
);

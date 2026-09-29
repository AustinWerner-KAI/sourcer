-- Fixes from the 29 Sep 2026 design audit.

-- 1. One candidacy per person per role, not per brief version, so editing a
--    brief can never create a second record and a second contact.
ALTER TABLE candidacy DROP CONSTRAINT candidacy_person_id_brief_id_key;
ALTER TABLE candidacy ADD CONSTRAINT candidacy_person_role_key UNIQUE (person_id, role_id);

-- 2. The whole sequence (first message and follow-ups) is approved once.
ALTER TABLE candidacy ADD COLUMN sequence_approved_by uuid REFERENCES app_user(id);
ALTER TABLE candidacy ADD COLUMN sequence_approved_at timestamptz;

-- 3. Owner release: the owner hands a person to a colleague, or ownership lapses
--    30 days after the last outbound touch with no reply.
ALTER TABLE candidacy ADD COLUMN owner_released_at timestamptz;

-- 4. Contact details, typed, so personal emails can be treated differently
--    from work emails.
CREATE TYPE contact_kind AS ENUM ('work_email', 'personal_email', 'phone');
CREATE TABLE contact (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  person_id   uuid NOT NULL REFERENCES person(id) ON DELETE CASCADE,
  kind        contact_kind NOT NULL,
  value       text NOT NULL,
  verified    boolean NOT NULL DEFAULT false,
  source      text NOT NULL,          -- 'apollo', 'pdl', 'manual'
  created_at  timestamptz NOT NULL DEFAULT now(),
  UNIQUE (org_id, kind, value)
);

-- 5. Do-not-contact list. Survives deletion of the person so an opt-out is
--    honoured forever. Holds only the identifier needed to match, nothing else.
CREATE TABLE do_not_contact (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id      uuid NOT NULL REFERENCES org(id),
  identifier  text NOT NULL,          -- lower-cased email, LinkedIn URL or E.164 phone
  reason      text NOT NULL,          -- 'opt_out', 'erasure_request'
  created_at  timestamptz NOT NULL DEFAULT now(),
  UNIQUE (org_id, identifier)
);

-- 6. Kill switch for all sending and paid calls, per organisation.
ALTER TABLE org ADD COLUMN sending_paused boolean NOT NULL DEFAULT false;
ALTER TABLE org ADD COLUMN paid_calls_paused boolean NOT NULL DEFAULT false;

-- Sprint 5: the Recruitly link (D10). Roles from Recruitly jobs, the Recruitly
-- check at shortlist, and handing candidates over to Recruitly.

-- Calls made to Recruitly per organisation per day, so the plan's daily
-- limit is never reached. Counted before each call.
CREATE TABLE recruitly_usage (
  org_id  uuid NOT NULL REFERENCES org(id),
  day     date NOT NULL,
  calls   int  NOT NULL DEFAULT 0,
  PRIMARY KEY (org_id, day)
);

-- The Recruitly company behind a client, and the job behind a role.
ALTER TABLE client ADD COLUMN recruitly_company_id text;
CREATE UNIQUE INDEX client_recruitly ON client (org_id, recruitly_company_id)
    WHERE recruitly_company_id IS NOT NULL;
ALTER TABLE role ADD COLUMN recruitly_job_id text;
-- What the screens call the job, e.g. "Senior IAM Engineer (J-1042)".
ALTER TABLE role ADD COLUMN recruitly_job_label text;
CREATE UNIQUE INDEX role_recruitly ON role (org_id, recruitly_job_id)
    WHERE recruitly_job_id IS NOT NULL;

-- What Recruitly knew about a person when last checked.
-- recruitly_id is set only for a sure match (same LinkedIn or email).
ALTER TABLE person ADD COLUMN recruitly_id text;
ALTER TABLE person ADD COLUMN recruitly_checked_at timestamptz;
-- In words, for the list. NULL when Recruitly has no one like them.
ALTER TABLE person ADD COLUMN recruitly_note text;
ALTER TABLE person ADD COLUMN recruitly_check_failed boolean NOT NULL DEFAULT false;
-- Who owns them in Recruitly, so sending a colleague's person asks first.
ALTER TABLE person ADD COLUMN recruitly_owner_id text;
-- Marked "do not contact" in Recruitly: never shortlisted or contacted.
ALTER TABLE person ADD COLUMN recruitly_dnc boolean NOT NULL DEFAULT false;

-- One handover per candidacy. Each step is saved as it succeeds, so a retry
-- carries on where it stopped and never creates a second Recruitly record.
CREATE TABLE recruitly_handover (
  candidacy_id  uuid PRIMARY KEY REFERENCES candidacy(id) ON DELETE CASCADE,
  org_id        uuid NOT NULL REFERENCES org(id),
  by_user       uuid REFERENCES app_user(id),
  candidate_id  text,
  pipeline_id   text,
  noted         boolean NOT NULL DEFAULT false,
  -- Set before creating the record and before posting the note, so a retry
  -- after a lost answer never makes a second one without asking.
  create_attempted boolean NOT NULL DEFAULT false,
  note_attempted   boolean NOT NULL DEFAULT false,
  -- Held while a handover runs, so two clicks never run it twice. Every write
  -- during the run names its token, so a run that was taken over stops.
  claim_token   uuid,
  claimed_at    timestamptz NOT NULL DEFAULT now(),
  done_at       timestamptz
);

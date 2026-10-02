-- M2 outreach, part two: sending from each person's own Outlook (Kai, 2 Oct 2026).
--
-- Delegated Microsoft Graph permissions only (Mail.Send, Mail.ReadWrite,
-- offline_access): each person connects their own mailbox once. The refresh
-- token is encrypted with MAIL_TOKEN_KEY, which lives only in deploy/.env.

CREATE TABLE mailbox (
  user_id        uuid PRIMARY KEY REFERENCES app_user(id) ON DELETE CASCADE,
  org_id         uuid NOT NULL REFERENCES org(id),
  -- The mailbox's own address, lower-case. Must be the user's Sourcer email.
  address        text NOT NULL,
  -- AES-256-GCM: 12-byte nonce, then the ciphertext.
  refresh_token  bytea NOT NULL,
  connected_at   timestamptz NOT NULL DEFAULT now(),
  -- Mail received up to here has been checked for replies.
  checked_at     timestamptz,
  -- Why sending from this mailbox stopped, in words. NULL when it works.
  broken         text
);

-- A mailbox connection in progress, tied to the person who started it.
CREATE TABLE mail_connect (
  state       text PRIMARY KEY,
  verifier    text NOT NULL,
  user_id     uuid NOT NULL REFERENCES app_user(id) ON DELETE CASCADE,
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- Each email as Outlook knows it. The id is Graph's immutable id, so it stays
-- the same when the draft moves to Sent Items.
ALTER TABLE outreach_step ADD COLUMN graph_id text;
-- Set just before the send call. A step with this set and no sent_at is
-- checked against Outlook before anything else happens, so nothing goes twice.
ALTER TABLE outreach_step ADD COLUMN sending_since timestamptz;

-- Outlook refused this email this many times; it is tried again after
-- retry_after, and the sequence stops after three.
ALTER TABLE outreach_step ADD COLUMN failures int NOT NULL DEFAULT 0;
ALTER TABLE outreach_step ADD COLUMN retry_after timestamptz;

ALTER TABLE outreach ADD COLUMN conversation_id text;
-- The first reply, automatic reply or bounce, and whether someone has dealt with it.
ALTER TABLE outreach ADD COLUMN reply_kind text CHECK (reply_kind IN ('reply', 'auto', 'bounce'));
ALTER TABLE outreach ADD COLUMN replied_at timestamptz;
ALTER TABLE outreach ADD COLUMN reply_handled_at timestamptz;

-- First emails each person may send in one Dubai day. Follow-ups do not count.
ALTER TABLE org ADD COLUMN first_emails_per_day int NOT NULL DEFAULT 25
  CHECK (first_emails_per_day BETWEEN 0 AND 200);

CREATE INDEX outreach_sending ON outreach (status) WHERE status IN ('approved', 'active');

-- Sprint 1: Microsoft 365 sign-in (SRS F1, N3).

-- The Microsoft account id (object id). Once set, only that Microsoft account
-- can use this user; a reused email address never inherits someone's account.
ALTER TABLE app_user ADD COLUMN ms_oid text;
CREATE UNIQUE INDEX app_user_ms_oid ON app_user (ms_oid) WHERE ms_oid IS NOT NULL;

-- An admin can switch a user off; their sessions stop working at once.
ALTER TABLE app_user ADD COLUMN disabled_at timestamptz;

-- A sign-in in progress: the state and PKCE verifier sent to Microsoft.
-- Used once, and only within 10 minutes.
CREATE TABLE oauth_state (
  state       text PRIMARY KEY,
  verifier    text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- Signed-in sessions. Only a SHA-256 hash of the cookie value is stored, so a
-- database leak does not hand out working sessions. A session ends after 12
-- hours idle (expires_at slides forward on use) or 7 days in total.
CREATE TABLE user_session (
  token_hash          text PRIMARY KEY,
  org_id              uuid NOT NULL REFERENCES org(id),
  user_id             uuid NOT NULL REFERENCES app_user(id) ON DELETE CASCADE,
  created_at          timestamptz NOT NULL DEFAULT now(),
  expires_at          timestamptz NOT NULL,
  absolute_expires_at timestamptz NOT NULL
);
CREATE INDEX user_session_by_user ON user_session (user_id);

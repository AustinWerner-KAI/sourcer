-- Sprint 1: Microsoft 365 sign-in (SRS F1, N3).

-- The Microsoft account id (object id), so a changed email never splits a user.
ALTER TABLE app_user ADD COLUMN ms_oid text;
CREATE UNIQUE INDEX app_user_ms_oid ON app_user (ms_oid) WHERE ms_oid IS NOT NULL;

-- A sign-in in progress: the state and PKCE verifier sent to Microsoft.
-- Used once, and only within 10 minutes.
CREATE TABLE oauth_state (
  state       text PRIMARY KEY,
  verifier    text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- Signed-in sessions. Only a SHA-256 hash of the cookie value is stored, so a
-- database leak does not hand out working sessions.
CREATE TABLE user_session (
  token_hash  text PRIMARY KEY,
  org_id      uuid NOT NULL REFERENCES org(id),
  user_id     uuid NOT NULL REFERENCES app_user(id) ON DELETE CASCADE,
  created_at  timestamptz NOT NULL DEFAULT now(),
  expires_at  timestamptz NOT NULL
);
CREATE INDEX user_session_by_user ON user_session (user_id);

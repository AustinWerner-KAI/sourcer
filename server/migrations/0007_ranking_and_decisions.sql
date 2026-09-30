-- Sprint 3: rank, known check, shortlist and reject (SRS F6, F7, F9).

-- Skills from the provider, used only to rank. Never names or contact details.
ALTER TABLE person ADD COLUMN skills jsonb NOT NULL DEFAULT '[]';

-- When a candidacy was found and ranked, so the list keeps a stable order.
ALTER TABLE candidacy ADD COLUMN created_at timestamptz NOT NULL DEFAULT now();
ALTER TABLE candidacy ADD COLUMN ranked_at timestamptz;
ALTER TABLE candidacy ADD CONSTRAINT candidacy_tier CHECK (tier IS NULL OR tier IN ('A', 'B', 'C'));
ALTER TABLE candidacy ADD CONSTRAINT candidacy_rank CHECK (rank IS NULL OR rank BETWEEN 0 AND 100);
-- A rejection always has a reason.
ALTER TABLE candidacy ADD CONSTRAINT candidacy_reject_reason CHECK (state <> 'rejected' OR reason IS NOT NULL);
CREATE INDEX candidacy_role_state ON candidacy (role_id, state);

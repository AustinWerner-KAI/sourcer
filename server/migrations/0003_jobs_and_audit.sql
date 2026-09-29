-- Sprint 1: job recovery and audit lookups.

-- When a worker claims a job it stamps started_at, so a job left 'running' by a
-- crashed worker can be found and put back in the queue.
ALTER TABLE job ADD COLUMN started_at timestamptz;
CREATE INDEX job_running ON job (status, started_at) WHERE status = 'running';

-- The audit log is read newest first, per organisation.
CREATE INDEX audit_org_at ON audit (org_id, at DESC);

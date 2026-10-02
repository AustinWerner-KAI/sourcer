-- Open and closed roles (Kai, 2 Oct 2026). A closed role drops off the Roles
-- list's Open tab, cannot be searched, and sends no more emails: its
-- sequences stop at the next check before sending. Reopening allows
-- searching and new approvals again; stopped sequences stay stopped.
ALTER TABLE role ADD COLUMN closed_at timestamptz;
ALTER TABLE role ADD COLUMN closed_by uuid REFERENCES app_user(id);

-- The signature as it was when the emails were approved. Sending uses this
-- copy, so editing the signature in Settings never changes approved emails.
ALTER TABLE outreach ADD COLUMN signature text;

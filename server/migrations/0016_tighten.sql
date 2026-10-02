-- Tightening (Kai, 2 Oct 2026): when a count finds far too many people,
-- Claude tightens the brief towards the spec, quoting the spec for each
-- change. A brief confirmed from a tightening records which round it was, so
-- it stops after two. A brief confirmed any other way starts again at 0.
ALTER TABLE brief ADD COLUMN tighten_round smallint NOT NULL DEFAULT 0;

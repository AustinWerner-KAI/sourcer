-- The brief holds enough to aim a search: job titles, fewest years,
-- standards and certifications, and Claude's read of the spec.
ALTER TABLE brief ADD COLUMN titles jsonb NOT NULL DEFAULT '[]';
ALTER TABLE brief ADD COLUMN min_years integer CHECK (min_years BETWEEN 0 AND 40);
ALTER TABLE brief ADD COLUMN frameworks jsonb NOT NULL DEFAULT '[]';
ALTER TABLE brief ADD COLUMN certifications jsonb NOT NULL DEFAULT '[]';
ALTER TABLE brief ADD COLUMN analysis text NOT NULL DEFAULT '';

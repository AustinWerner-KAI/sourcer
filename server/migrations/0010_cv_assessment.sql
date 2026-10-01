-- CV assessment and the feedback loop (Kai, 1 Oct 2026).
--
-- A CV is kept as text only, with the name, emails, phones and links removed
-- before anything is stored or sent to Claude. The file itself is never kept.
-- Each upload is a new row, so feedback on older assessments stays and keeps
-- teaching the ranking.

CREATE TABLE cv (
  id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id             uuid NOT NULL REFERENCES org(id),
  person_id          uuid NOT NULL REFERENCES person(id) ON DELETE CASCADE,
  file_name          text NOT NULL,
  text               text NOT NULL,
  -- Claude's overall call across the roles assessed: what to do next.
  call               text NOT NULL DEFAULT '',
  model              text,
  uploaded_by        uuid REFERENCES app_user(id),
  created_at         timestamptz NOT NULL DEFAULT now(),
  assessed_at        timestamptz,
  recruitly_noted_at timestamptz
);
CREATE INDEX cv_person ON cv (person_id, created_at DESC);

CREATE TABLE cv_assessment (
  id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  org_id         uuid NOT NULL REFERENCES org(id),
  cv_id          uuid NOT NULL REFERENCES cv(id) ON DELETE CASCADE,
  role_id        uuid NOT NULL REFERENCES role(id) ON DELETE CASCADE,
  score          int NOT NULL CHECK (score BETWEEN 1 AND 10),
  fit_title      text NOT NULL,
  matches        jsonb NOT NULL DEFAULT '[]',
  gaps           jsonb NOT NULL DEFAULT '[]',
  flags          jsonb NOT NULL DEFAULT '[]',
  questions      jsonb NOT NULL DEFAULT '[]',
  -- The profile rank (0 to 100) for this role when the CV was assessed.
  profile_score  int,
  created_at     timestamptz NOT NULL DEFAULT now(),
  -- The resourcer's verdict, which feeds later rankings and assessments.
  feedback       text CHECK (feedback IN ('accurate', 'too_high', 'too_low')),
  feedback_score int CHECK (feedback_score BETWEEN 1 AND 10),
  feedback_note  text NOT NULL DEFAULT '',
  -- Positions (from 0) of the questions marked vital.
  vital          int[] NOT NULL DEFAULT '{}',
  feedback_by    uuid REFERENCES app_user(id),
  feedback_at    timestamptz,
  UNIQUE (cv_id, role_id)
);
CREATE INDEX cv_assessment_feedback ON cv_assessment (org_id, feedback_at DESC) WHERE feedback IS NOT NULL;

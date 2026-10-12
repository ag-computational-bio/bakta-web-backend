-- Creation time of the recorded workflow, so an older attempt never
-- replaces a newer one.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS workflow_created_at timestamptz;

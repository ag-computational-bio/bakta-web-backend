-- The Kubernetes resourceVersion of the stored state, so the startup
-- reconcile only fetches completed workflows that changed.
ALTER TABLE workflows ADD COLUMN IF NOT EXISTS resource_version text;

-- Finished jobs are served from this table: the SHA-256 hash of the job
-- secret (never the secret), the result kind and the Argo workflow uid.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS secret_hash text;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS result_kind text;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS argo_uid uuid;

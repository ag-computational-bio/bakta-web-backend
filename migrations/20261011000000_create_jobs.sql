CREATE TABLE IF NOT EXISTS jobs (
    id uuid PRIMARY KEY,
    environment text NOT NULL,
    workflow_kind text NOT NULL,
    api_version text NOT NULL,
    status text NOT NULL,
    outcome text,
    name text,
    genus text,
    species text,
    strain text,
    sequence_count integer,
    total_length bigint,
    config jsonb,
    bakta_version text,
    bakta_db_version text,
    baktfold_version text,
    baktfold_db_version text,
    workflow_name text,
    created_at timestamptz NOT NULL,
    submitted_at timestamptz,
    started_at timestamptz,
    finished_at timestamptz,
    runtime_seconds double precision,
    queue_seconds double precision,
    error_message text,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS jobs_environment_created_at_idx ON jobs (environment, created_at);
CREATE INDEX IF NOT EXISTS jobs_status_idx ON jobs (status);

CREATE TABLE IF NOT EXISTS workflows (
    namespace text NOT NULL,
    name text NOT NULL,
    job_id uuid,
    environment text NOT NULL,
    workflow_kind text,
    template text,
    phase text NOT NULL,
    created_at timestamptz,
    started_at timestamptz,
    finished_at timestamptz,
    message text,
    suspended boolean NOT NULL,
    origin text,
    labels jsonb NOT NULL,
    gone_at timestamptz,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (namespace, name)
);

CREATE INDEX IF NOT EXISTS workflows_environment_phase_idx ON workflows (namespace, environment, phase);
CREATE INDEX IF NOT EXISTS workflows_finished_at_idx ON workflows (finished_at);
CREATE INDEX IF NOT EXISTS workflows_job_id_idx ON workflows (job_id);

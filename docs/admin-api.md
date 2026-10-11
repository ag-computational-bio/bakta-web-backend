# Admin API and job statistics

This document is the contract between the backend and the admin UI (hidden `/admin` page in bakta-web-ui).
Both sides implement exactly what is described here. Changes to the contract are made in this file first.

## Overview

- The backend records every job in a separate Postgres database (the statistics database).
- Admins log in through Keycloak. The admin UI sends the Keycloak access token to the backend.
- The admin API offers statistics, a job list with details, and Argo operations (retry, resubmit, stop, resume, delete, bulk actions), so the Argo UI is no longer needed for daily work.
- Both features are optional. If their configuration is missing, the backend works as before and the admin API answers `503 Service Unavailable` with a short message.

## Configuration (backend environment variables)

| Variable | Example | Meaning |
|---|---|---|
| `ENVIRONMENT` | `production` | Environment name written to every job record and to the `bakta-environment` workflow label. Default `production`. |
| `STATS_DATABASE_URL` | `postgres://bakta_stats:...@bakta-stats-postgres:5432/bakta_stats` | Statistics database. If unset, job recording and the statistics endpoints are disabled. |
| `ADMIN_OIDC_ISSUER` | `https://auth.bakta.computational.bio/realms/bakta` | Keycloak realm issuer. If unset, the admin API is disabled. |
| `ADMIN_OIDC_CLIENT_ID` | `bakta-admin` | Keycloak client used by the admin UI. Tokens must have this value in `azp` (or in `aud`). |
| `ADMIN_GROUP` | `bakta-admin` | Group name that must appear in the token `groups` claim. Default `bakta-admin`. |

## Authentication

- Keycloak realm `bakta`, public client `bakta-admin` with Authorization Code flow and PKCE (no client secret).
- Redirect URIs: `https://bakta.computational.bio/admin/*`, `https://staging.bakta.computational.bio/admin/*`, `http://localhost:5173/admin/*`.
- A group membership mapper adds the claim `groups` (list of group names, without full path) to the access token, like the existing Grafana client.
- Every request to `/api/admin/*` must send `Authorization: Bearer <access token>`.
- The backend validates the token: RS256 signature against the realm JWKS (`<issuer>/protocol/openid-connect/certs`, cached, refreshed when an unknown `kid` appears), `iss` equals `ADMIN_OIDC_ISSUER`, `exp` and `nbf` are valid, `azp` or `aud` contains `ADMIN_OIDC_CLIENT_ID`, and `groups` contains `ADMIN_GROUP`.
- Missing or invalid token: `401`. Valid token without the admin group: `403`.
- Admin actions are logged with the user name (`preferred_username`) and the job or workflow they affect.

## Statistics database

Migrations are embedded in the backend and run at startup. Table `jobs`, one row per job:

| Column | Type | Notes |
|---|---|---|
| `id` | uuid, primary key | Job ID |
| `environment` | text | `production` or `staging` |
| `workflow_kind` | text | `bakta`, `bakta_baktfold`, `bakta_proteins`, `baktfold` |
| `api_version` | text | `v1` or `v2` |
| `status` | text | `created`, `submitted`, `running`, `succeeded`, `failed`, `error`, `deleted`, `expired` |
| `outcome` | text, null | Final workflow result: `succeeded`, `failed` or `error`. Kept when the status later becomes `deleted` or `expired`. |
| `name` | text, null | Job name as entered by the user |
| `genus`, `species`, `strain` | text, null | Organism as entered by the user |
| `sequence_count` | integer, null | Number of input sequences (from `input_stats`, see below) |
| `total_length` | bigint, null | Total input length in bp |
| `config` | jsonb, null | All input options of the start request, without secrets |
| `bakta_version`, `bakta_db_version`, `baktfold_version`, `baktfold_db_version` | text, null | Versions used for the job |
| `workflow_name` | text, null | Current Argo workflow name |
| `created_at` | timestamptz | Job init |
| `submitted_at`, `started_at`, `finished_at` | timestamptz, null | Start request, first running state, final state |
| `runtime_seconds` | double precision, null | `finished_at - started_at` |
| `queue_seconds` | double precision, null | `started_at - submitted_at` |
| `error_message` | text, null | Short reason for failed jobs, if known |
| `updated_at` | timestamptz | Last change |

Rules:

- The job secret, upload URLs and IP addresses are never stored.
- Writes never block or fail a user request. If the database is unreachable, the backend logs a warning and continues.
- Records are kept forever. Expired or deleted jobs keep their row with status `expired` or `deleted`.
- Statistics count finished, succeeded, failed and error jobs by `outcome`, so a job still counts after it was deleted or has expired.

### Input statistics in the V2 start request

The V2 start request gets an optional top level field, ignored by older clients:

```json
{
  "job": { "id": "...", "secret": "..." },
  "workflow_kind": "bakta",
  "config": { "...": "..." },
  "input_stats": { "sequence_count": 3, "total_length": 5594605 }
}
```

The web UI fills it from the parsed FASTA input. The backend stores it in `sequence_count` and `total_length`.

## Common query parameters

- `environment`: `production` (default), `staging` or `all`.
- `from`, `to`: RFC 3339 timestamps, filter on `created_at` (`from` inclusive, `to` exclusive). Default: all time.
- Times in responses are RFC 3339 strings in UTC. Durations are seconds as numbers.

## Endpoints

All paths start with `/api/admin`. All responses are JSON. Errors use `{ "error": "message" }`.

### `GET /me`

```json
{ "username": "jdoe", "name": "Jane Doe", "email": "jdoe@example.org", "groups": ["bakta-admin"] }
```

`name` and `email` are null when the token has no such claim.

### `GET /stats/summary?environment&from&to`

```json
{
  "jobs_total": 1234,
  "jobs_finished": 1200,
  "jobs_succeeded": 1150,
  "jobs_failed": 40,
  "jobs_error": 10,
  "jobs_active": 4,
  "success_rate": 0.958,
  "runtime_seconds": { "avg": 412.5, "p50": 380.0, "p90": 720.0, "p99": 1500.0 },
  "queue_seconds": { "avg": 12.3, "p50": 8.0, "p90": 30.0, "p99": 90.0 },
  "total_input_bp": 5123456789,
  "by_workflow_kind": [
    { "workflow_kind": "bakta", "jobs_total": 1000, "jobs_succeeded": 960, "runtime_seconds": { "avg": 400.0, "p50": 370.0, "p90": 700.0, "p99": 1400.0 } }
  ]
}
```

`jobs_finished`, `jobs_succeeded`, `jobs_failed` and `jobs_error` count by `outcome`. `jobs_active` counts jobs with status `submitted` or `running`.
`success_rate` is `jobs_succeeded / jobs_finished` (null when nothing finished). Percentiles use `percentile_cont`.
The duration values (`avg`, `p50`, `p90`, `p99`) are null when no matching job has that duration.

### `GET /stats/timeseries?environment&from&to&interval&group_by`

- `interval`: `day`, `week`, `month` (default) or `year`. Buckets start at `date_trunc(interval, created_at)` in UTC.
- `group_by`: `none` (default), `workflow_kind`, `status` or `api_version`.

```json
{
  "interval": "month",
  "group_by": "workflow_kind",
  "buckets": [
    { "period_start": "2026-09-01T00:00:00Z", "group": "bakta", "jobs_total": 120, "jobs_succeeded": 115, "jobs_failed": 4, "jobs_error": 1, "runtime_p50": 390.0, "runtime_p90": 700.0, "runtime_avg": 420.0, "total_input_bp": 600000000, "input_p50_bp": 4800000.0, "input_avg_bp": 5000000.0 }
  ]
}
```

With `group_by=none`, `group` is `null`. Buckets without jobs are left out; the UI fills gaps.
`runtime_p50`, `runtime_p90` and `runtime_avg` use succeeded jobs only (by `outcome`) and are null when the bucket has no succeeded jobs with a runtime.
`input_p50_bp` and `input_avg_bp` use all jobs of the bucket with a `total_length` and are null when there are none.

### `GET /stats/versions?environment&from&to`

```json
{ "items": [ { "bakta_version": "1.12.2", "bakta_db_version": "6.0", "baktfold_version": "0.3.0", "jobs_total": 300, "first_seen": "...", "last_seen": "..." } ] }
```

### `GET /stats/runtimes?environment&from&to&workflow_kind`

`workflow_kind` is optional, comma separated. Runtime statistics only use succeeded jobs (by `outcome`, so jobs that were later deleted or expired still count) with a non-null `runtime_seconds`.

```json
{
  "jobs": 812,
  "by_workflow_kind": [
    { "workflow_kind": "bakta", "jobs": 600, "min": 95.0, "p10": 180.0, "p25": 240.0, "p50": 380.0, "p75": 520.0, "p90": 720.0, "p95": 900.0, "p99": 1500.0, "max": 4200.0, "avg": 412.5, "stddev": 210.3 }
  ],
  "histogram": [ { "min_seconds": 0, "max_seconds": 120, "jobs": 12 } ],
  "by_input_size": [ { "min_bp": 0, "max_bp": 1000000, "jobs": 40, "runtime_p50": 150.0, "runtime_p90": 300.0, "runtime_avg": 170.0 } ],
  "seconds_per_mbp": { "p50": 70.5, "avg": 80.1 },
  "queue_seconds": { "avg": 12.3, "p50": 8.0, "p90": 30.0, "p99": 90.0, "max": 600.0 },
  "slowest": [ { "id": "...", "workflow_kind": "bakta", "organism": "Escherichia coli Sakai", "total_length": 5594605, "runtime_seconds": 4200.0, "created_at": "..." } ]
}
```

- `histogram` uses fixed buckets in seconds: 0-120, 120-300, 300-600, 600-1200, 1200-1800, 1800-3600, 3600-7200, 7200-14400, and 14400 and more (`max_seconds` null for the last). Empty buckets are included with `jobs: 0`, so there are always 9 buckets.
- `by_input_size` uses fixed buckets in bp: under 1 Mbp, 1-2, 2-4, 4-6, 6-8, 8-12 Mbp, and 12 Mbp and more (`max_bp` null for the last). Only jobs with `total_length`. Empty buckets are included with zero jobs and null percentiles.
- Bucket ranges include `min` and exclude `max`.
- `seconds_per_mbp`: runtime divided by `total_length / 1e6`, over jobs with `total_length > 0`.
- `queue_seconds` covers all jobs with a non-null `queue_seconds` in the filter, not only succeeded ones.
- `slowest`: top 10 by runtime, newest first on ties.
- All percentile and average fields are null when there are no matching jobs; `jobs` is then 0.

### `GET /stats/inputs?environment&from&to&workflow_kind`

Input statistics over jobs with a non-null `total_length`. `workflow_kind` is optional, comma separated.

```json
{
  "jobs": 1200,
  "total_bp": 5123456789,
  "total_length": { "min": 2000.0, "p10": 1500000.0, "p25": 2800000.0, "p50": 4800000.0, "p75": 5600000.0, "p90": 6500000.0, "p99": 11000000.0, "max": 15000000.0, "avg": 4600000.0 },
  "sequence_count": { "min": 1, "p50": 3, "p90": 120, "p99": 800, "max": 4000, "avg": 42.5 },
  "size_histogram": [ { "min_bp": 0, "max_bp": 1000000, "jobs": 40 } ],
  "sequence_histogram": [ { "min_sequences": 1, "max_sequences": 1, "jobs": 300 } ],
  "by_workflow_kind": [ { "workflow_kind": "bakta", "jobs": 900, "total_bp": 4000000000, "p50_bp": 4900000.0, "avg_bp": 4700000.0, "p50_sequences": 3.0 } ],
  "largest": [ { "id": "...", "workflow_kind": "bakta", "organism": "...", "total_length": 15000000, "sequence_count": 4000, "status": "succeeded", "created_at": "..." } ]
}
```

- `size_histogram`: the same 7 bp buckets as `by_input_size` in `GET /stats/runtimes`, always all 7.
- `sequence_histogram`: fixed buckets 1, 2-5, 6-20, 21-100, 101-500, 501-2000, 2001 and more (`max_sequences` is inclusive and null for the last), always all 7, over jobs with a non-null `sequence_count`.
- `sequence_count` statistics use jobs with a non-null `sequence_count`.
- `largest`: top 10 by `total_length`.
- Percentile and average fields are null when there are no matching jobs.

### `GET /jobs?environment&from&to&status&workflow_kind&workflow_phase&q&limit&offset&sort`

- `status`, `workflow_kind`: comma separated lists.
- `workflow_phase`: comma separated list of live Argo phases (`Pending`, `Running`, `Succeeded`, `Failed`, `Error`). Only jobs with a live workflow in one of these phases match.
- `q`: matches the job ID prefix, organism (genus, species, strain) or name, case insensitive.
- `limit` default 50, max 500. `offset` default 0.
- `sort`: `created_at` (default, newest first), `-created_at` (oldest first), `runtime_seconds`, `-runtime_seconds`, `total_length`, `-total_length`. For `runtime_seconds` and `total_length` the plain value sorts ascending and the `-` prefix descending. Empty values come last in both directions.

```json
{
  "total": 1234,
  "items": [
    {
      "id": "...", "environment": "production", "workflow_kind": "bakta", "status": "succeeded", "outcome": "succeeded",
      "organism": "Escherichia coli Sakai", "sequence_count": 3, "total_length": 5594605,
      "created_at": "...", "finished_at": "...", "runtime_seconds": 401.2,
      "workflow_name": "bakta-job-1.12.2-...", "workflow_phase": "Succeeded"
    }
  ]
}
```

`organism` joins genus, species and strain with spaces, or is null.
`workflow_name` and `workflow_phase` come from the workflow list the backend polls from Argo (no Argo request per row). Both are null when no live workflow exists for the job.

### `GET /jobs/{id}`

The full database record plus the live Argo state (null if the workflow no longer exists):

```json
{
  "job": { "id": "...", "environment": "production", "workflow_kind": "bakta", "api_version": "v2", "status": "succeeded", "outcome": "succeeded",
           "name": "...", "genus": "...", "species": "...", "strain": "...", "sequence_count": 3, "total_length": 5594605,
           "config": { }, "bakta_version": "1.12.2", "bakta_db_version": "6.0", "baktfold_version": null, "baktfold_db_version": null,
           "workflow_name": "bakta-job-1.12.2-...", "created_at": "...", "submitted_at": "...", "started_at": "...", "finished_at": "...",
           "runtime_seconds": 401.2, "queue_seconds": 9.1, "error_message": null, "updated_at": "..." },
  "workflow": {
    "name": "bakta-job-1.12.2-...", "phase": "Succeeded", "started_at": "...", "finished_at": "...", "message": null, "suspended": false,
    "nodes": [ { "name": "...", "display_name": "bakta", "type": "Pod", "phase": "Succeeded", "pod_name": "...", "started_at": "...", "finished_at": "...", "message": null } ]
  }
}
```

Unknown job: `404`.

### `GET /jobs/{id}/logs`

Same shape as the V2 logs response (`workflow_kind`, `stages` with `stage`, `status`, `content`), without the job secret.

### `POST /jobs/{id}/retry`

Argo retry of a failed or errored workflow (reruns failed steps). `409` if the workflow is not in `Failed` or `Error`. Response: `{ "workflow_name": "...", "phase": "Running" }`.

### `POST /jobs/{id}/resubmit`

Argo resubmit: a new workflow with the same parameters. The job record points to the new `workflow_name` and goes back to `submitted`. Response: `{ "workflow_name": "...", "phase": "Pending" }`.

### `POST /jobs/{id}/stop`

Argo terminate of a running workflow. `409` if the workflow is not running. Response: `{ "workflow_name": "...", "phase": "Failed" }`.

### `POST /jobs/{id}/resume`

Argo resume of a suspended workflow. `409` if the workflow is not suspended. Response: `{ "workflow_name": "...", "phase": "Running" }`.

A workflow is suspended when it is `Running` and has `spec.suspend` set to true or a node of type `Suspend` that is still running. The `suspended` field in `GET /workflows` items and in the `workflow` object of `GET /jobs/{id}` tells the UI when resume applies.

### `DELETE /jobs/{id}?purge_data=false`

Deletes the Argo workflow (Argo removes its pods) and sets the job status to `deleted`. With `purge_data=true` the job's S3 objects are deleted too. Response: `{ "deleted_workflow": true, "purged_objects": 12 }`.

### `GET /workflows?environment&phase&orphans`

For this endpoint and for `POST /workflows/bulk`, `environment` defaults to the backend's own `ENVIRONMENT`.

Live Argo workflows of this backend, including ones without a job record. `phase`: comma separated (`Pending`, `Running`, `Succeeded`, `Failed`, `Error`).

```json
{ "items": [ { "name": "...", "job_id": "...", "workflow_kind": "bakta", "phase": "Failed", "template": "bakta-job-1.12.2", "environment": "production", "started_at": "...", "finished_at": "...", "message": "...", "suspended": false } ] }
```

`job_id` is null when the workflow has no `jobid` label. `workflow_kind` is null when the workflow has no `workflow-kind` label.
With `orphans=true`, only workflows without a matching job record are returned: no `jobid` label, or a job ID that is not in the statistics database. This needs the statistics database (`503` without it).

### `POST /workflows/bulk`

Runs one action on all live workflows that match a filter.

```json
{
  "filter": {
    "q": "bakta-job-1.12",
    "phases": ["Failed", "Error"],
    "workflow_kinds": ["bakta"],
    "finished_before_hours": 24,
    "environment": "production"
  },
  "action": "delete",
  "dry_run": true,
  "workflow_names": ["..."]
}
```

- `filter` fields are all optional and combine with AND. `q` matches the workflow name or job ID, case insensitive substring. `finished_before_hours` keeps only workflows that finished more than that many hours ago. `environment` defaults to the backend's own environment, `all` is allowed.
- `action`: `delete`, `stop` (Argo terminate), `retry` (Argo retry), `resume` (Argo resume of suspended workflows), `resubmit` (Argo resubmit; the job record then points to the new workflow).
- `dry_run` defaults to `true`: return the matching workflows and, per workflow, whether the action applies, without changing anything.
- `workflow_names`: when given (a real run after a dry run), only workflows in this list that still match the filter are processed.
- Applicability: `stop` for `Pending` and `Running`, `retry` for `Failed` and `Error`, `resume` only for suspended workflows, `resubmit` for any finished workflow (`Succeeded`, `Failed`, `Error`), `delete` for any.
- Workflows are processed one by one. One failure does not stop the others. Each action is logged with the admin user name.

Response:

```json
{
  "dry_run": true,
  "action": "delete",
  "items": [
    { "name": "...", "job_id": "...", "workflow_kind": "bakta", "phase": "Failed", "finished_at": "...", "applicable": true, "result": null, "error": null }
  ]
}
```

In a dry run `result` is null. In a real run `result` is `done`, `skipped` (not applicable) or `failed` with `error` set.

## Workflow labels

Every submitted workflow gets the label `bakta-environment=<ENVIRONMENT>` in addition to the existing labels (`jobid`, `origin`, ...). The workflow list and bulk actions only act on workflows of the backend's own environment unless `environment=all` is given.
They only ever act on Bakta Web workflows: workflows with a `bakta-environment` label, or, for workflows created before that label existed, workflows with a `jobid` label that were created from a Bakta Web workflow template (`bakta-job-*`, `bakta-proteins-job-*`, `bakta-baktfold-job-*`, `baktfold-job-*`). `environment=all` only widens the environment, never this rule. Other workflows in the shared Argo namespace are never listed or changed.

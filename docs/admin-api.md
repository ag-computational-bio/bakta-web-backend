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
| `STATS_DATABASE_URL` | `postgres://bakta_stats:...@bakta-stats-postgres:5432/bakta_stats` | Statistics database. If unset, job recording and the statistics endpoints are disabled, and finished jobs are kept in memory for 35 days instead of being served from the database (see "Workflow watch"). |
| `MANAGE_LEGACY_PRODUCTION_WORKFLOWS` | `false` | Default `false`. When `true`, a backend whose `ENVIRONMENT` is not `production` also manages workflows without a `bakta-environment` label whose derived environment is `production` (see "Environment of a workflow"). Only for the time while production still runs the old backend, which records nothing. It must be `false` on staging **before or together with** the upgrade of the production backend to this version, see "Operator note" below. |
| `KUBERNETES_API_URL` | `https://127.0.0.1:6443` | Kubernetes API server for the workflow watch. Default in a pod: `https://$KUBERNETES_SERVICE_HOST:$KUBERNETES_SERVICE_PORT`. One of them must be set. The token is `ARGO_TOKEN`: its service account needs `get`, `list` and `watch` on `workflows.argoproj.io` in `ARGO_NAMESPACE`. |
| `KUBERNETES_CA_FILE` | `/etc/kube/ca.crt` | CA certificate of the API server. Default: the service account CA of the pod when it exists, else the system certificates. |
| `ADMIN_OIDC_ISSUER` | `https://auth.bakta.computational.bio/realms/bakta` | Keycloak realm issuer. If unset, the admin API is disabled. |
| `ADMIN_OIDC_CLIENT_ID` | `bakta-admin` | Keycloak client used by the admin UI. Tokens must have this value in `azp` (or in `aud`). |
| `ADMIN_GROUP` | `bakta-admin` | Group name that must appear in the token `groups` claim. Default `bakta-admin`. |
| `DATA_RETENTION_DAYS` | `30` | Days after which the job data in S3 (inputs, results and archived logs) is deleted. Default `30`. The admin API uses it to tell which jobs still have their data. |

## Authentication

- Keycloak realm `bakta`, public client `bakta-admin` with Authorization Code flow and PKCE (no client secret).
- Redirect URIs: `https://bakta.computational.bio/admin/*`, `https://staging.bakta.computational.bio/admin/*`, `http://localhost:5173/admin/*`.
- A group membership mapper adds the claim `groups` (list of group names, without full path) to the access token, like the existing Grafana client.
- Every request to `/api/admin/*` must send `Authorization: Bearer <access token>`.
- The backend validates the token: RS256 signature against the realm JWKS (`<issuer>/protocol/openid-connect/certs`, cached, refreshed when an unknown `kid` appears), `iss` equals `ADMIN_OIDC_ISSUER`, `exp` and `nbf` are valid, `azp` or `aud` contains `ADMIN_OIDC_CLIENT_ID`, and `groups` contains `ADMIN_GROUP`.
- Missing or invalid token: `401`. Valid token without the admin group: `403`.
- Admin actions are logged with the user name (`preferred_username`) and the job or workflow they affect.

## Request durations

- The private metrics endpoint (`METRICS_SOCKET_ADDR`, default port 9090, path `/metrics`) has the histogram `http_request_duration_seconds` for all API requests, including `/api/admin/*`. Labels: `route` (the route template, for example `/api/admin/jobs/{id}`, or `unmatched`), `method` and `status`.
- Requests that take 500 ms or longer are logged as a warning `slow_request` with route, method, status and `duration_ms`.

## Statistics database

Migrations are embedded in the backend and run at startup. Table `jobs`, one row per job:

| Column | Type | Notes |
|---|---|---|
| `id` | uuid, primary key | Job ID |
| `environment` | text | `production` or `staging`, see "Environment of a workflow" below |
| `origin` | text, null | Request origin: the `Origin` header of the start request without `https://`, other characters than letters, digits, `_` and `.` replaced by `_`, or `Unknown` when the header is missing. Same value as the `origin` workflow label. Null for jobs that were never started or polled since migration `20261012000000_add_jobs_origin`. |
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
| `submitted_at`, `started_at`, `finished_at` | timestamptz, null | Start, retry or resubmit request of the current attempt; Argo `status.startedAt` and `status.finishedAt` of the current workflow |
| `runtime_seconds` | double precision, null | `finished_at - started_at`, null when one of them is null |
| `queue_seconds` | double precision, null | `started_at - submitted_at`, null when one of them is null or `started_at` is before `submitted_at` |
| `error_message` | text, null | Short reason for failed jobs, if known |
| `secret_hash` | text, null | Hex SHA-256 hash of the job secret. Set at job init, else from the `secret` label of the workflow. Never sent by any API. |
| `result_kind` | text, null | `bakta`, `bakta_proteins` or `baktfold`, for the result files of a finished job |
| `argo_uid` | uuid, null | Argo uid of the current workflow, for the logs of archived workflows |
| `workflow_created_at` | timestamptz, null | Creation time of the recorded workflow. A workflow update never replaces the recorded workflow with an older one. |
| `updated_at` | timestamptz | Last change |

Rules:

- The job secret, upload URLs and IP addresses are never stored. Only the SHA-256 hash of the job secret is stored (`secret_hash`), see "Workflow watch".
- Writes go through one background writer, in order. Failed writes are handled by the kind of error:
  - Transient errors (no connection, pool timeout, I/O, serialization failure, deadlock, lock timeout, server shutdown, write timeout): the write is repeated in place with a pause of 1 to 10 seconds as long as it takes; later writes wait.
  - Writes that keep the state of jobs (job init, workflow updates and completions, resubmits, deletes and expiry) that fail with a permanent error (constraint, data or schema error), or still fail after 5 attempts with another error, are parked: kept in memory and logged once (`job_record_write_parked` with the job ID and the error). Later writes of the same job wait behind it, writes of other jobs go on. A workflow update of several jobs is split first, so only the bad job is parked. Parked writes are tried again every 5 minutes and whenever a new write of their job arrives; `job_record_write_unparked` is logged when they succeed. They are never dropped. A new workflow update of a parked job replaces its parked workflow updates (it carries the newer full state), so a state that is recorded again while it cannot be written is kept only once.
  - Statistics-only writes (start request details, retry times, environment and origin corrections) are tried three times and then dropped with a warning.
  - Metrics: `job_record_writes_parked` (number of parked writes) and `job_record_writer_stuck_seconds` (how long the writer has retried a transient error, 0 while it works). `GET /ready` answers `503` when the writer has been stuck on transient errors for more than 2 minutes. Parked writes of single jobs do not affect readiness.
  - The queue is kept in memory: after a restart, the reconcile writes the final states of finished jobs again.
- Only two operations wait for their write: a delete (user or admin) answers only after the tombstone (`status = 'deleted'`) is written, else `503` (`The job database is not available, try again later`); the write is still repeated, and a repeated delete waits for it again, also when the backend already removed the job from memory. And the reconcile after a list counts as complete only when the final states of the jobs that are not in memory are written. All other user requests do not wait for writes.
- Records are kept forever. Expired or deleted jobs keep their row with status `expired` or `deleted`.
- Statistics count finished, succeeded, failed and error jobs by `outcome`, so a job still counts after it was deleted or has expired.
- Runtime and queue time measure the current attempt: a retry or resubmit sets `submitted_at` to its own time, and the times of the current workflow describe the new attempt (Argo resets `startedAt` and clears `finishedAt` on retry). The earlier `outcome` is kept until the new attempt finishes.

### How the job state is kept in sync

The workflow state in a job record follows the job's current workflow in Argo:

- When a watched workflow changes or completes (see "Workflow watch"), the backend takes the current workflow of its job: the workflow recorded for the job, else the one recorded in the database if it is known, else the newest by creation time and then by name. The text below calls such an update a poll, as before.
- From that workflow it sets `workflow_name`, `status` (`Pending` is `submitted`, `Running` is `running`, a final phase is `succeeded`, `failed` or `error`), `started_at` (null while `submitted`), `finished_at`, `runtime_seconds`, `queue_seconds`, `error_message` and, for a final phase, `outcome`.
- A record is only written when it differs from this state. The comparison is made against the database, so a wrong or outdated record is corrected by the next update of the job, or at the latest by the reconcile after the next start (see "Workflow watch").
- A poll also sets `environment` (see below) and `origin` (from the `origin` label; a workflow without this label keeps the stored value).
- Records with status `deleted` or `expired` are not changed by polls, except `environment` and `origin`. A poll also sets these two columns on the existing records of deleted and expired jobs whose workflow Argo still lists, including workflows that finished more than 35 days ago. It never creates a record for such a job and changes no other column.
- A resubmit records the new workflow for the job at once, also for a job that is only in the database (for example after a restart). So the next poll takes the new workflow as current, even before the database write of the resubmit is done.
- Other writes only set what polls cannot see: the job init, the start request (input options, input statistics, versions, `submitted_at`), the request time of a retry or resubmit (`submitted_at`), user and admin deletes, and expiry. They never overwrite the workflow state of a newer poll. A resubmit only points the record to the new workflow (and resets its state to `submitted`) when no poll has seen that workflow yet.

### Live workflows

Table `workflows` keeps the live Bakta Web workflows for `GET /workflows` and the bulk preview, so these do not need to list all workflows from Argo. One row per workflow, primary key `namespace` and `name`:

| Column | Type | Notes |
|---|---|---|
| `namespace`, `name` | text | Argo namespace and workflow name |
| `job_id` | uuid, null | `jobid` label, null when missing or not a UUID |
| `environment` | text | Derived environment, see "Environment of a workflow" below |
| `workflow_kind`, `origin` | text, null | `workflow-kind` and `origin` labels |
| `template` | text, null | Name of the workflow template |
| `phase` | text | Argo phase |
| `created_at`, `started_at`, `finished_at` | timestamptz, null | Argo creation time, `status.startedAt` and `status.finishedAt` |
| `message` | text, null | Argo `status.message` |
| `suspended` | boolean | See `POST /jobs/{id}/resume` |
| `labels` | jsonb | All workflow labels |
| `resource_version` | text, null | Kubernetes `resourceVersion` of the stored state, so the reconcile after a start fetches only changed workflows |
| `gone_at` | timestamptz, null | Time the workflow was no longer listed or was deleted by an admin. Such rows are hidden and deleted after one hour. |
| `updated_at` | timestamptz | Time of the state in the row |

How the table is written:

- The workflow watch (see "Workflow watch") writes every Bakta Web workflow (see "Workflow labels" below) that is added or changes, about one second after the change. A completed workflow is written once with its final state. A workflow that is deleted gets `gone_at`. A row is only written when it differs.
- After each full list (at startup and after a re-list), the completed workflows are written once and all rows of workflows that Kubernetes no longer has get `gone_at`.
- `suspended` comes from `spec.suspend`. No workflow template has a `Suspend` step, and Kubernetes objects have no nodes.
- Admin actions (retry, resubmit, stop, resume, delete, bulk runs) write the workflow that Argo returns, or set `gone_at` for deleted workflows, right away.
- A row is never set to a state that is older than the one it has, so a change seen before an admin action does not undo the action's write.
- So the data is about one second old while the watch is connected. The backfill fills the table with all Bakta Web workflows (see "Backfill").

### Environment of a workflow

Production and staging backends watch the same Argo namespace. The backend takes the environment of a workflow from its labels:

1. The `bakta-environment` label, when it is a valid environment name.
2. Else `staging` when the workflow has an `api-version` label. Before workflows were labelled with their environment, only the staging backend set `api-version`.
3. Else `staging` when the `origin` label starts with `staging.`, `http_localhost` or `http_127.0.0.1` (the staging and local development UIs).
4. Else `production` (workflows of the old production backend).

Workflow updates correct the `environment` of existing records with this rule.

Each backend manages only the jobs of its own environment: the job state in memory, the user API (also for finished jobs read from the database), and the job record writes after workflow changes, completions and the reconcile. Workflows and records of the other environment are left to the other backend, so two backends that watch the same namespace and write the same database never write the same job. The one exception is `MANAGE_LEGACY_PRODUCTION_WORKFLOWS` (default off): with it, a backend of another environment also manages unlabelled workflows that count as `production`. The `workflows` table and the admin API still cover all environments. An admin action on a workflow of another environment updates the shared job record, but this backend keeps no job state in memory for it (and drops old state it may have), because it does not follow that workflow. A delete through the other backend only writes the shared tombstone: every 15 seconds, and right after the watch sees a workflow deleted, each backend checks the records of the jobs in its memory and removes those with status `deleted` like a local delete (they go on the list of deleted jobs, kept completions are dropped). A job that is being started, deleted or resubmitted on this backend is left to that action.

Independently, a job record never switches from its recorded workflow to an older attempt: a workflow update only replaces the recorded workflow with one that was created at the same time or later (`workflow_created_at`). And a finished job in memory whose workflow is no longer watched takes a newer attempt of the job as current, when that attempt was created after the job finished. The workflow list and the bulk actions use the same rule to filter workflows by environment.

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

### Backfill

`bakta-web-backend backfill` imports the job history once and exits. The Argo server only lists recent workflows, so the backfill reads all `Workflow` objects from the Kubernetes API (in-cluster service account, `get` and `list` on `workflows.argoproj.io` in `ARGO_NAMESPACE`). It needs `STATS_DATABASE_URL` and the S3 variables. The GitOps repository has a Kubernetes Job for it.

- All Bakta Web workflows are written to the `workflows` table, with the state they had before the list. A row that the backend changed later is kept, also a gone one.
- For the job records, only the newest workflow of each job is used.
- The `secret` label is replaced by its hash (`secret_hash`) right after reading. The secret is never kept.
- Missing jobs are inserted. Existing records only get columns that are still null. Existing values, the environment and the status are never changed.
- Environment, origin, status, outcome, times, runtime, queue time and error message are derived like in the poll. A job past the 35 day job retention gets status `expired` with its `outcome`.
- Organism and `config` come from the Bakta command line of the workflow, in the shape the start request of its API version is stored. Bakta and Baktfold versions come from the template name.
- For succeeded `bakta` and `bakta_baktfold` jobs, `total_length`, `sequence_count` and `bakta_db_version` come from Bakta's summary `jobs/<id>/results/result.txt` in S3, if it still exists.
- `secret_hash`, `result_kind` and `argo_uid` are filled like the other columns.
- Running it again changes nothing that is already filled. Errors of single jobs are logged and skipped. It logs progress every 1000 jobs and a final summary (`inserted`, `filled`, `skipped`, `s3_missing`, `errors`).

### Operator note: `MANAGE_LEGACY_PRODUCTION_WORKFLOWS`

Turn `MANAGE_LEGACY_PRODUCTION_WORKFLOWS` off on staging **before or in the same rollout as** the production backend upgrade to this version. An upgraded production backend owns all workflows that count as `production`, including the unlabelled old ones. If staging still has the switch on, both backends own these jobs: both write their records, serve them and apply admin changes, and their states can overwrite each other. Order of the rollout:

1. Set `MANAGE_LEGACY_PRODUCTION_WORKFLOWS=false` (or remove it) on staging and restart staging.
2. Deploy the production backend of this version.

Rolling both in one change is fine, as long as staging does not run with the switch on next to an upgraded production backend.

## Workflow watch

The backend no longer polls the Argo server. It reads Workflow objects from the Kubernetes API (`/apis/argoproj.io/v1alpha1/namespaces/<ARGO_NAMESPACE>/workflows`) and keeps only workflows that are not completed in memory:

- At startup it lists the workflows with the label selector `workflows.argoproj.io/completed!=true` in pages of 500, then watches them from the resource version of the list, with bookmarks. Changes are applied in batches, about one second after the first change.
- A dropped watch is resumed from the last resource version (events and bookmarks) with a backoff of 1 to 60 seconds. Only when Kubernetes answers `410 Gone` (or sends an `ERROR` event with code 410) are the workflows listed again. Workflows missing from the new list count as deleted. Re-lists are logged (`workflow_watch_expired`).
- A workflow that completes gets the label `workflows.argoproj.io/completed=true` and leaves the watched set. Kubernetes then sends a `DELETED` event that has the state before this change, not the final state. So the backend fetches this one workflow from the Kubernetes API: if it exists with the completed label, it completed, and its final state is applied and written once to the `jobs` and `workflows` tables. If it does not exist, it was deleted: its row gets `gone_at` and the job keeps its last known state, as before. If the fetch fails, it is repeated with a pause of 1 to 60 seconds until Kubernetes answers. Until then the workflow stays watched with its last state. A completion that cannot be applied at once (the job is being started, deleted or resubmitted, or a database lookup failed) is kept and applied on a later pass. It is dropped when the same workflow is watched again with a newer resource version, for example after an Argo retry. A `MODIFIED` event that already has the completed label is taken as a completion without a fetch.
- After each list, the completed workflows (`workflows.argoproj.io/completed=true`) are listed once, page by page, with only their metadata (`Accept: application/json;as=PartialObjectMetadataList;g=meta.k8s.io;v=v1`). A workflow whose `workflows` row is missing or has another `resource_version`, or whose job record is not final yet (current workflow with the status and `finished_at` of the row, and with `secret_hash`, `result_kind` and `argo_uid`, unless the job is deleted, expired or has a newer workflow), is fetched in full: one by one, or as a full page when more than 50 of a page changed (for example at the first start before the backfill). If the API server refuses the metadata form, the full list is used. Records that the database does not have in this final state are written (the database compares, so unchanged records are not written). This covers workflows that completed while the backend was down. For each job, the newest completed workflow from all pages becomes current when the job has no recorded workflow or when it was created after the recorded one (a record without `workflow_created_at`, for example an old or backfilled one, counts as older; the creation time is filled then). This covers a resubmit whose record update was lost with a restart. Deleted jobs are not written, and a job that is being started, deleted or resubmitted is left to the next reconcile run. After that, no completed workflow is fetched again. When the reconcile fails (the list, the database lookup, a write or a single fetch), it is repeated with a pause of 1 to 60 seconds until it is complete, or until a newer list starts another one. A failed write to the `workflows` table after a change, and a failed `gone_at` write after an admin delete (a deleted completed workflow sends no watch event), is kept with its time and repeated every 15 seconds. In the cluster, the metadata list of about 40,000 completed workflows is about 70 MB, the full list about 333 MB.
- Every 15 seconds the backend drops expired jobs, checks unresolved start requests, applies jobs that were skipped (reserved, or a failed database lookup) and deletes `workflows` rows that are gone for an hour. While the watch is not connected, it only drops expired jobs, so the last known state stays.
- Readiness: `GET /ready` on the public listener (`SOCKET_ADDR`) answers `503` until the first list of workflows was applied and the completed workflows were reconciled once completely, and while the statistics database (when configured) does not answer within 2 seconds. Then it answers `200`. It is meant for a Kubernetes readiness probe. There is no separate liveness endpoint.
- Health: `workflow_watch_last_event_timestamp_seconds` (last event, bookmark or watch start), `workflow_last_list_timestamp_seconds` (last full list), `workflow_watch_errors_total` (failed lists, watches and fetches) and `workflow_relists_total` on the metrics endpoint. They replace `argo_poll_errors_total`.

Finished jobs:

- With the statistics database, a finished job leaves memory once its record has the final state (status and workflow name). Until then the final state is recorded again every 15 seconds. Memory only holds jobs with a watched workflow, jobs being started, deleted or resubmitted, and unresolved start requests.
- Every hour, records of finished jobs (`succeeded`, `failed`, `error`) whose finish (or start) is older than 35 days get status `expired`, like jobs in memory before.
- User requests (V1 and V2 job list, results, logs, delete and start) for a job that is not in memory read its record. Records with status `created`, `deleted` or `expired`, and finished records older than 35 days, are not found. The given secret is hashed and compared with `secret_hash` in constant time. A record without `secret_hash` denies every secret. When the database lookup fails, these requests answer `503` (`The job database is not available, try again later`) instead of reporting the job as missing or deleted. Logs of finished jobs are read from the Argo archive with `argo_uid`.
- Without the statistics database, the completed workflows of the list after startup are applied to memory, and finished jobs stay in memory for 35 days as before.

What still calls Argo:

- The Argo server: submit, single workflow actions (retry, resubmit, stop, resume, delete), logs and node details of one workflow (also the admin job page), the label lookup of a job's workflows after an unclear start or before a delete without a known workflow name, and the single workflow fetches of a real bulk run.
- The Kubernetes API: the list and watch of workflows that are not completed, one fetch per workflow that leaves the watched set, and the list of completed workflows after each list.
- Nothing lists all workflows from the Argo server.

## Data retention

Job data in S3 (inputs, results and archived logs) is deleted `DATA_RETENTION_DAYS` days after the job. The backend keeps job state for 35 days, and the statistics database keeps job records forever. So a job can exist while its S3 data is gone.

Job records (items of `GET /jobs` and the `job` object of `GET /jobs/{id}`) have two extra fields:

- `data_expires_at`: timestamp, the reference time plus `DATA_RETENTION_DAYS` days. The reference time is `finished_at`, or `created_at` when the job never finished (a retry or resubmit clears `finished_at` until the new attempt ends).
- `data_available`: boolean, true while now is before `data_expires_at`. At exactly `data_expires_at` it is false.

When `data_available` is false, the admin API refuses everything that needs S3 data. The rules are listed at the endpoints below (`retry`, `resubmit`, `DELETE`, `logs`) and at `POST /workflows/bulk`.
For a job without a database record, the backend uses the time of its in-memory job state (last update, or start) as reference time.

## Unresolved start requests

A start request can fail in a way that leaves open whether Argo created the workflow (for example a timeout or an unreadable answer). The backend then looks for a workflow with the job's `jobid` label. If it finds one, the job uses it. If the lookup fails or finds nothing, the job is marked as unresolved, because Argo may still be creating the workflow.

While a job is unresolved, start and delete requests for it are refused with `409` and `The result of an earlier start request is not known yet, try again later`. The workflow updates resolve the job. Each update (after a change, and every 15 seconds while the watch is connected) counts as a poll:

- A polled workflow list that contains a workflow with the job's `jobid` label resolves it at once. The job then uses that workflow. A poll that runs while the job is being started, deleted or resubmitted skips the job and keeps the mark. A later poll resolves it.
- Otherwise it stays unresolved until at least 3 polls fetched after the mark found no such workflow and at least 10 minutes have passed since the mark. Then a new start is allowed.

An admin resubmit (single or bulk) can fail in the same way: Argo may have created the new workflow, but the answer timed out or could not be read. The backend then marks the job as unresolved too, without a lookup. The same `409` message is used. While the mark is set, start, delete, retry and resubmit requests for the job are refused, and bulk `retry` and `resubmit` skip the job with that message as `reason`. The old workflow of the job never resolves the mark:

- A polled workflow of the job resolves it only when it is not the resubmitted workflow and Argo created it at or after the resubmit request (30 seconds of tolerance for clock differences). The newest such workflow becomes the job's workflow, and the job record points to it like after a successful resubmit.
- Otherwise the same rule applies: 3 polls over 10 minutes without such a workflow. The job then keeps the old workflow.

## Common query parameters

- `environment`: `production` (default), `staging` or `all`.
- `from`, `to`: RFC 3339 timestamps, filter on `created_at` (`from` inclusive, `to` exclusive). Default: all time.
- Times in responses are RFC 3339 strings in UTC. Durations are seconds as numbers.

## Endpoints

All paths start with `/api/admin`. All responses are JSON. Errors use `{ "error": "message" }`. This includes malformed requests (bad path or query values, malformed JSON, unknown enum values), which answer `400` or `422`.

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
    { "workflow_kind": "bakta", "jobs_total": 1000, "jobs_finished": 990, "jobs_succeeded": 960, "jobs_failed": 25, "jobs_error": 5, "runtime_seconds": { "avg": 400.0, "p50": 370.0, "p90": 700.0, "p99": 1400.0 } }
  ],
  "by_origin": [
    { "origin": "bakta.computational.bio", "jobs_total": 900 },
    { "origin": null, "jobs_total": 12 }
  ]
}
```

`by_origin` counts jobs per `origin`, ordered by `jobs_total` (descending) and then by `origin`. Jobs without an origin form one item with `origin` null.

`jobs_finished`, `jobs_succeeded`, `jobs_failed` and `jobs_error` count by `outcome`, also in the `by_workflow_kind` items. `jobs_active` counts jobs with status `submitted` or `running`.
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
  "overall": { "jobs": 812, "min": 95.0, "p10": 180.0, "p25": 240.0, "p50": 380.0, "p75": 520.0, "p90": 720.0, "p95": 900.0, "p99": 1500.0, "max": 4200.0, "avg": 412.5, "stddev": 210.3 },
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

- `overall` has the same fields as a `by_workflow_kind` item, without `workflow_kind`, over all matching succeeded jobs (all selected workflow kinds together). Its values are null and `jobs` is 0 when there are no such jobs.
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
      "id": "...", "environment": "production", "origin": "bakta.computational.bio", "workflow_kind": "bakta", "status": "succeeded", "outcome": "succeeded",
      "organism": "Escherichia coli Sakai", "sequence_count": 3, "total_length": 5594605,
      "created_at": "...", "finished_at": "...", "runtime_seconds": 401.2,
      "data_expires_at": "...", "data_available": true,
      "workflow_name": "bakta-job-1.12.2-...", "workflow_phase": "Succeeded"
    }
  ]
}
```

`organism` joins genus, species and strain with spaces, or is null.
`workflow_name` and `workflow_phase` come from the backend's workflow state in memory for jobs with a watched workflow, else from the `workflows` table row of the job's recorded workflow (no Argo request per row). Both are null when no live workflow exists for the job.

### `GET /jobs/{id}`

The full database record plus the live Argo state (null if the workflow no longer exists):

```json
{
  "job": { "id": "...", "environment": "production", "origin": "bakta.computational.bio", "workflow_kind": "bakta", "api_version": "v2", "status": "succeeded", "outcome": "succeeded",
           "name": "...", "genus": "...", "species": "...", "strain": "...", "sequence_count": 3, "total_length": 5594605,
           "config": { }, "bakta_version": "1.12.2", "bakta_db_version": "6.0", "baktfold_version": null, "baktfold_db_version": null,
           "workflow_name": "bakta-job-1.12.2-...", "created_at": "...", "submitted_at": "...", "started_at": "...", "finished_at": "...",
           "runtime_seconds": 401.2, "queue_seconds": 9.1, "error_message": null, "updated_at": "...",
           "data_expires_at": "...", "data_available": true },
  "workflow": {
    "name": "bakta-job-1.12.2-...", "phase": "Succeeded", "started_at": "...", "finished_at": "...", "message": null, "suspended": false,
    "nodes": [ { "name": "...", "display_name": "bakta", "type": "Pod", "phase": "Succeeded", "pod_name": "...", "started_at": "...", "finished_at": "...", "message": null } ]
  }
}
```

Unknown job: `404`.

### `GET /jobs/{id}/logs`

Same shape as the V2 logs response (`workflow_kind`, `stages` with `stage`, `status`, `content`), without the job secret.

`410 Gone` with `{ "error": "Logs were deleted after the retention period" }` when `data_available` is false and the logs would come from the archive. Logs of a live (not archived) workflow are still served while Argo has its pods.

### `POST /jobs/{id}/retry`

Argo retry of a failed or errored workflow (reruns failed steps). `409` if the workflow is not in `Failed` or `Error`. `409` with `{ "error": "Input data was deleted after the retention period" }` when `data_available` is false (the inputs no longer exist). Response: `{ "workflow_name": "...", "phase": "Running" }`.

### `POST /jobs/{id}/resubmit`

Argo resubmit: a new workflow with the same parameters. The job record points to the new `workflow_name` and goes back to `submitted`. `409` with `{ "error": "Input data was deleted after the retention period" }` when `data_available` is false. Response: `{ "workflow_name": "...", "phase": "Pending" }`.
`409` with `{ "error": "Job is busy, try again later" }` while the job is being started, deleted or resubmitted. While the resubmit runs, start and delete requests for the job are refused with `409`.
A retry needs no such check: it reruns the same workflow and creates no new one, so a delete at the same time removes it.
`409` with `{ "error": "Job was deleted" }` when the job was deleted: it is in the backend's in-memory list of deleted and expired jobs, or it is not in memory and its record has status `deleted`. A resubmit never adds a deleted job back.
When the resubmit answer times out or cannot be read, the response is `502` and the job is marked as unresolved (see Unresolved start requests). Retry and resubmit answer `409` with `{ "error": "The result of an earlier start request is not known yet, try again later" }` while a job is unresolved.

### `POST /jobs/{id}/stop`

Argo terminate of a running workflow. `409` if the workflow is not running. Response: `{ "workflow_name": "...", "phase": "Failed" }`.

### `POST /jobs/{id}/resume`

Argo resume of a suspended workflow. `409` if the workflow is not suspended. Response: `{ "workflow_name": "...", "phase": "Running" }`.

A workflow is suspended when it is `Running` and has `spec.suspend` set to true or a node of type `Suspend` that is still running. The `suspended` field in `GET /workflows` items and in the `workflow` object of `GET /jobs/{id}` tells the UI when resume applies.

### `DELETE /jobs/{id}?purge_data=false`

Deletes the Argo workflow (Argo removes its pods) and sets the job status to `deleted`. With `purge_data=true` the job's S3 objects are deleted too. Response: `{ "deleted_workflow": true, "purged_objects": 12 }`.
A workflow can be live and archived at the same time. So every delete (admin or user) deletes the live workflow by name, and also the archive record when the job is archived. A workflow or archive record that Argo does not find counts as already deleted.
When `data_available` is false, the S3 listing and purge are skipped and `purged_objects` is `0`. The workflow is still deleted and the job is still marked `deleted`.
After a delete (admin or user) or an expiry, the workflow updates never add the job back, even when its workflow list was fetched before the delete or Argo still lists the workflow for a while. The backend keeps the IDs of deleted and expired jobs in memory for 35 days, the time it keeps job state. After that, a poll treats any finished workflow of the job as expired. The list is lost on a restart. With the statistics database, a poll checks the jobs that are not in memory in one query and does not add a job back whose record has status `deleted` (it goes back on the list instead). If that query fails, the poll adds no job that is not in memory, and a later poll adds them. Without the statistics database, a poll after a restart can add a deleted job back while Argo still lists one of its workflows. This is rare, because a delete removes the live workflow.
`409` while the job is being started or resubmitted. While the delete runs, a start request for the job is refused with `409`.
`409` with `{ "error": "The result of an earlier start request is not known yet, try again later" }` while an earlier start request may have created a workflow that the backend has not found yet (see Unresolved start requests). The user delete endpoints (`DELETE /api/v1/job/delete`, `DELETE /api/v2/job/delete`) answer `409` in the same case.
When the backend knows no workflow name for the job (for example a start request created a workflow, but the backend restarted before a poll found it), the delete first looks up live workflows with the job's `jobid` label in Argo and deletes them. If that lookup fails, the delete answers `503` and changes nothing: no workflow is deleted, no S3 data is purged and the job is not marked `deleted`. The user delete endpoints do the same lookup for jobs that are in memory and also answer `503` when it fails. A job that is not in memory and has a record (a finished job) is deleted like a job in memory after its secret was checked against `secret_hash`: its workflow by name and its archive record by `argo_uid`. A job that is neither in memory nor in the database is not looked up: the user delete answers `200` without contacting Argo.

### `GET /workflows?environment&phase&orphans&limit&offset`

For this endpoint and for `POST /workflows/bulk`, `environment` defaults to the backend's own `ENVIRONMENT`. The backend keeps the Bakta Web workflows whose environment, derived as in "Environment of a workflow", matches. So workflows without a `bakta-environment` label are found too.

With the statistics database, this endpoint and the bulk preview read the `workflows` table (see "Live workflows"), newest first. Without it, they read the workflows that the backend watches (only workflows that are not completed). A real bulk run takes the same workflows and fetches each one from Argo right before acting on it.

Live Argo workflows of this backend, including ones without a job record. `phase`: comma separated (`Pending`, `Running`, `Succeeded`, `Failed`, `Error`).

```json
{ "items": [ { "name": "...", "job_id": "...", "workflow_kind": "bakta", "phase": "Failed", "template": "bakta-job-1.12.2", "environment": "production", "started_at": "...", "finished_at": "...", "message": "...", "suspended": false } ], "total": 1 }
```

The list has one page of the matching workflows, newest first: `limit` (default 1000, at most 5000) workflows after skipping `offset` (default 0). `total` is the number of all matching workflows, also those outside the page, after the `phase` and `orphans` filters. Use `total` for counts, not the length of `items`.

`job_id` is null when the workflow has no `jobid` label. `workflow_kind` is null when the workflow has no `workflow-kind` label. `environment` is the derived environment.
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
  "workflow_names": ["..."],
  "limit": 1000
}
```

- `filter` fields are all optional and combine with AND. `q` matches the workflow name or job ID, case insensitive substring. `finished_before_hours` keeps only workflows that finished more than that many hours ago. `environment` defaults to the backend's own environment, `all` is allowed.
- `action`: `delete`, `stop` (Argo terminate), `retry` (Argo retry), `resume` (Argo resume of suspended workflows), `resubmit` (Argo resubmit; the job record then points to the new workflow).
- `dry_run` defaults to `true`: return the matching workflows and, per workflow, whether the action applies, without changing anything.
- `workflow_names`: when given (a real run after a dry run), only workflows in this list that still match the filter are processed. A real run fetches each of these workflows from Argo right before acting, and checks the filter and the applicability against that state. A workflow that Argo no longer has, or that no longer matches, is left out. A failed fetch gives an item with `result` `failed`.
- Applicability: `stop` for `Pending` and `Running`, `retry` for `Failed` and `Error`, `resume` only for suspended workflows, `resubmit` for any finished workflow (`Succeeded`, `Failed`, `Error`), `delete` for any workflow that is not `Pending` or `Running`.
- `delete` does not apply to `Pending` and `Running` workflows: `applicable` is false and `reason` is `Still running, stop it first`. A real run skips them.
- `retry` and `resubmit` only apply to the job's current workflow. An older workflow of a job that was resubmitted is not applicable (`reason` names the current workflow).
- When the job's current workflow cannot be determined (for example the job is not in memory after a restart and the statistics database fails or is not configured, or the job's workflow is not recorded yet), `retry` and `resubmit` do not apply: `applicable` is false and `reason` is `Current workflow unknown, try again later`. A workflow without a `jobid` label, or whose job has no record, counts as current. The single `retry` and `resubmit` endpoints answer `503` when the job lookup fails.
- `retry` and `resubmit` do not apply when the input data is gone: `applicable` is false and `reason` is `Input data deleted after the retention period`. The reference time is the job's `finished_at` or `created_at` (see Data retention). For a workflow without a job record, it is the workflow's finish time or, if it has none, its creation time.
- `resubmit` does not apply to a workflow of a deleted job (same rule as the single resubmit): `applicable` is false and `reason` is `Job was deleted`.
- In a real run, `resubmit` is skipped while the job is being started, deleted or resubmitted: `applicable` is false and `reason` is `Job is busy, try again later`. A dry run does not check this. A real `resubmit` reserves the job before it checks which workflow is current, and keeps it reserved until the result is recorded. While a bulk run holds the reservation, other start, delete and resubmit requests for the job are refused with `409`.
- `limit` (default 1000, at most 5000): at most this many matching workflows are listed (dry run) or processed (real run), newest first. The response has `total`, the number of all matching workflows, and `truncated`, true when more workflows match than `items` has. Applicability is checked for the listed workflows only.
- Workflows are processed one by one. One failure does not stop the others. Each action is logged with the admin user name.

Response:

```json
{
  "dry_run": true,
  "action": "delete",
  "items": [
    { "name": "...", "job_id": "...", "workflow_kind": "bakta", "phase": "Failed", "finished_at": "...", "applicable": true, "result": null, "error": null, "reason": null }
  ],
  "total": 1,
  "truncated": false
}
```

In a dry run `result` is null. In a real run `result` is `done`, `skipped` (not applicable) or `failed` with `error` set. `reason` tells why the action does not apply, and is null when it applies.

## Workflow labels

Every submitted workflow gets the label `bakta-environment=<ENVIRONMENT>` in addition to the existing labels (`jobid`, `origin`, ...). The workflow list and bulk actions only act on workflows of the backend's own environment unless `environment=all` is given. Workflows without this label get their environment from the other labels (see "Environment of a workflow").
They only ever act on Bakta Web workflows: workflows with a `bakta-environment` label, or, for workflows created before that label existed, workflows with a `jobid` label that were created from a Bakta Web workflow template (`bakta-job-*`, `bakta-proteins-job-*`, `bakta-baktfold-job-*`, `baktfold-job-*`). `environment=all` only widens the environment, never this rule. Other workflows in the shared Argo namespace are never listed or changed.

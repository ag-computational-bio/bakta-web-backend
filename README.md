<h1 align="center">Bakta-Web backend</h1>
<p align="center">Backend to schedule Bakta jobs on a Kubernetes cluster.</p>

<p align="center"><a href="https://github.com/ag-computational-bio/bakta-web-backend" target="_blank"><img src="https://img.shields.io/badge/version-v1.1.0-blue?style=for-the-badge&logo=none"/></a>&nbsp;<a href="https://github.com/ag-computational-bio/bakta-web-backend" target="_blank"></a>&nbsp;<img src="https://img.shields.io/badge/license-gpl-red?style=for-the-badge&logo=none" alt="license" /></p>

# Concept
The Bakta-Web backend implements a simple job scheduling system for the Bakta-Web UI. It requires an S3 compatible object storage and a Kubernetes cluster with Argo Workflows to run the jobs. Jobs can be submitted via the API.

The corresponding repositories can be found here:
- [Bakta](https://github.com/oschwengers/bakta)
- [UI](https://github.com/ag-computational-bio/bakta-web-ui)

# Deployment
## Requirements
- S3-compatible object storage
- Kubernetes cluster
- Argo Workflows

Required workflow templates:
- V1: `bakta-job-{BAKTA_VERSION}`
- V2: `bakta-job-{BAKTA_VERSION}`
- V2: `bakta-proteins-job-{BAKTA_VERSION}`
- V2: `bakta-baktfold-job-{BAKTA_VERSION}-{BAKTFOLD_VERSION}`
- V2: `baktfold-job-{BAKTFOLD_VERSION}`

The container accepts the following settings via env-vars or `.env`:

- `SOCKET_ADDR=127.0.0.1:8080`
- `METRICS_SOCKET_ADDR=127.0.0.1:9090`
- `ARGO_TOKEN=token`
- `ARGO_URL=https://argo.example.com`
- `ARGO_NAMESPACE=argo`
- `S3_ACCESS_KEY=access_key`
- `S3_SECRET_KEY=secret_key`
- `S3_BUCKET=bucket`
- `S3_ENDPOINT=https://s3.example.com`
- `BAKTA_VERSION=0.1.0`
- `DATABASE_VERSION=0.1.0`
- `BAKTFOLD_VERSION=0.1.0`
- `BAKTFOLD_DATABASE_VERSION=0.1.0`
- `BACKEND_VERSION=1.1.0`
- `KUBERNETES_API_URL=https://127.0.0.1:6443` (only outside a pod; in a pod the service variables are used)
- `KUBERNETES_CA_FILE=/path/to/ca.crt` (optional)
- `STATS_DATABASE_URL=postgres://...` (optional)
- `MANAGE_LEGACY_PRODUCTION_WORKFLOWS=false` (optional; see `docs/admin-api.md`)

Notes:
- `SOCKET_ADDR` is the public API listener.
- `METRICS_SOCKET_ADDR` is a separate private listener exposing only `/metrics`.
- Swagger UI is available on the public listener at `/swagger-ui`.
- `GET /ready` on the public listener answers `503` until the first workflow list was applied and the completed workflows were reconciled, and while the statistics database (when configured) does not answer, then `200`. Use it as the readiness probe.
- The backend watches the Argo workflows through the Kubernetes API with `ARGO_TOKEN`. Its service account needs `get`, `list` and `watch` on `workflows.argoproj.io` in `ARGO_NAMESPACE`.
- With `STATS_DATABASE_URL`, finished jobs are served from the database: status, results, logs and delete work for them as before, with the job secret checked against a stored hash. Without it, finished jobs are kept in memory for 35 days. See `docs/admin-api.md`, section "Workflow watch".

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

Notes:
- `SOCKET_ADDR` is the public API listener.
- `METRICS_SOCKET_ADDR` is a separate private listener exposing only `/metrics`.
- Swagger UI is available on the public listener at `/swagger-ui`.

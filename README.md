<h1 align="center">Bakta-Web backend</h1>
<p align="center">Backend to schedule Bakta jobs on a Kubernetes cluster.</p>


<p align="center"><a href="https://github.com/ag-computational-bio/bakta-web-backend" target="_blank"><img src="https://img.shields.io/badge/version-v0.6.3-blue?style=for-the-badge&logo=none"/></a>&nbsp;<a href="https://github.com/ag-computational-bio/bakta-web-backend" target="_blank"></a>&nbsp;<img src="https://img.shields.io/badge/license-gpl-red?style=for-the-badge&logo=none" alt="license" /></p>

# Concept
The Bakta-Web backend implements a simple job scheduling system for the Bakta-Web UI. It requires an S3 compatible object storage, a Kubernetes cluster with ArgoWorkflows to run the jobs. Jobs can be submitted via an API. The API is a simple [REST API](https://api.bakta.computational.bio/). The corresponding repositories can be found here:
- [Bakta](https://github.com/oschwengers/bakta)
- [UI](https://github.com/ag-computational-bio/bakta-web-ui)

# Deployment
## Requirements
- S3-compatible object storage
- Kubernetes-Cluster
- Argo Workflows with a pre-configured WorkflowTemplate named `bakta-job-{BAKTA_VERSTION}`

The Container accepts the following settings via env-vars (or .env file):

- `SOCKET_ADDR=127.0.0.1:8080`
- `ARGO_TOKEN=token`
- `ARGO_URL=https://argo.example.com`
- `ARGO_NAMESPACE=argo`
- `S3_ACCESS_KEY=access_key`
- `S3_SECRET_KEY=secret_key`
- `S3_BUCKET=bucket`
- `S3_ENDPOINT=https://s3.example.com`
- `BAKTA_VERSION=0.1.0`
- `DATABASE_VERSION=0.1.0`
- `BACKEND_VERSION=0.2.0`

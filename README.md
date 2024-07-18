<h1 align="center">Bakta-Web backend</h1>
<p align="center">Backend to schedule Bakta jobs on a Kubernetes cluster.</p>


<p align="center"><a href="https://github.com/ag-computational-bio/bakta-web-backend" target="_blank"><img src="https://img.shields.io/badge/version-v0.2.0-blue?style=for-the-badge&logo=none"/></a>&nbsp;<a href="https://github.com/ag-computational-bio/bakta-web-backend" target="_blank"></a>&nbsp;<img src="https://img.shields.io/badge/license-gpl-red?style=for-the-badge&logo=none" alt="license" /></p>

# Concept
The Bakta-Web backend implements a simple job scheduling system for the Bakta-Web UI. It requires an S3 compatible object storage, a MongoDB and a Kubernetes cluster to run the jobs. Jobs can be submitted via an API. The API is defined using [gRPC](https://grpc.io/) and pregenerated builds are available for Golang. In addition the API can be queried using a JSON-over-REST API that is generated from the gRPC definitions using [grpc-gateway](https://github.com/grpc-ecosystem/grpc-gateway). The corresponding repositories can be found here:
- [Bakta](https://github.com/oschwengers/bakta)
- [UI](https://github.com/ag-computational-bio/bakta-web-ui)

# Deployment
## Requirements
- S3-compatible object storage
- Kubernetes-Cluster
- Argo Workflows

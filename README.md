<h1 align="center">Bakta-Web backend</h1>
<p align="center">Backend to schedule Bakta jobs on a Kubernetes cluster.</p>


<p align="center"><a href="https://github.com/ag-computational-bio/bakta-web-backend" target="_blank"><img src="https://img.shields.io/badge/version-v0.1.0-blue?style=for-the-badge&logo=none"/></a>&nbsp;<a href="https://github.com/ag-computational-bio/bakta-web-backend" target="_blank"><img src="https://img.shields.io/badge/Go-1.16+-00ADD8?style=for-the-badge&logo=go" alt="go version" /></a>&nbsp;<img src="https://img.shields.io/badge/license-apache_2.0-red?style=for-the-badge&logo=none" alt="license" /></p>

# Concept
The Bakta-Web backend implements a simple job scheduling system for the Bakta-Web UI. It requires an S3 compatible object storage, a MongoDB and a Kubernetes cluster to run the jobs. Jobs can be submitted via an API. The API is defined using [gRPC](https://grpc.io/) and pregenerated builds are available for Golang. In addition the API can be queried using a JSON-over-REST API that is generated from the gRPC definitions using [grpc-gateway](https://github.com/grpc-ecosystem/grpc-gateway). The corresponding repositories can be found here:
- [Bakta](https://github.com/oschwengers/bakta)
- [UI](https://github.com/ag-computational-bio/bakta-web-ui)
- [API](https://github.com/ag-computational-bio/bakta-web-api)
- [Go-API](https://github.com/ag-computational-bio/bakta-web-api-go)
- [JSON-over-REST Gateway](https://github.com/ag-computational-bio/bakta-web-gateway)

# Deployment
## Requirements
- S3-compatible object storage
- MongoDB
- Kubernetes-Cluster

## Configuration
The backend is configured mainly via a config file that needs to be mounted into the running pod of the application using a config map. The secrets for accessing the MongoDB and the object storage are mounted as environment variables via Kubernetes secrets. In addition a properly configured service-account is required. An example configuration file can be found in `config/config.yaml`, kubernetes deployments files for the deployment can be found in the `kube` directory. The service-account requires basic CRUD rights on the batch/Job resource.



### Configuration file

| Parameter                    | Description                                     |
|------------------------------|-------------------------------------------------|
| `Objectstorage.S3.UserBucket`| Bucket to store the input and output data       |
| `Objectstorage.S3.DBBucket`  | Bucket to store the bakta database              |
| `Objectstorage.S3.BaseKey`   | Key prefix to store the input and output data   |
| `Database.MongoHost`         | Hostname of the used MongoDB                    |
| `Database.MongoDBName`       | Name of the used database inside MongoDB        |
| `Database.MongoUser`         | Username for MongoDB authentication             |
| `Database.MongoAuthSource`   | Authentication database                         |
| `Database.MongoPort`         | Port of the MongoDB                             |
| `UpdateService.Name`         | Servicename that handles the job status updates |
| `UpdateService.Port`         | Serviceport that handles the job status updates |
| `ExpiryTime`                 | Defines how long a job should be kept around    |
| `JobContainer`               | Registry link to the job container              |
| `InCluster`                  | Indicates if the app is running inside a cluster|
| `Testing`                    | Indicates if the bakta test db should be used   |


### Environment variables
| Parameter                    | Description                                     |
|------------------------------|-------------------------------------------------|
| `MongoPassword`              | Password for the MongoDB user                   |
| `AWS_ACCESS_KEY_ID`          | S3 access key                                   |
| `AWS_SECRET_ACCESS_KEY`      | S3 secret key                                   |
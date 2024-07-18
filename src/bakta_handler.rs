use crate::{api_structs::VersionResponse, argo::client::ArgoClient, s3_handler::S3Handler};

pub struct BaktaHandler {
    pub argo_client: ArgoClient,
    pub s3_handler: S3Handler,
    pub version: VersionResponse,
}

impl BaktaHandler {
    pub fn new(
        argo_token: String,
        argo_url: String,
        argo_namespace: String,
        s3_access_key: String,
        s3_secret_key: String,
        bucket: String,
        endpoint: String,
        bakta_version: String,
        database_version: String,
        backend_version: String,
    ) -> Self {
        let argo_client = ArgoClient::new(argo_token, argo_url, argo_namespace);
        let s3_handler = S3Handler::new(s3_access_key, s3_secret_key, bucket, endpoint);
        BaktaHandler {
            argo_client,
            s3_handler,
            version: VersionResponse {
                tool: bakta_version,
                db: database_version,
                backend: backend_version,
            },
        }
    }
}

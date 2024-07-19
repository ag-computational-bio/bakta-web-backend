use anyhow::{anyhow, Result};
use reqsign::{AwsCredential, AwsV4Signer};
use reqwest::Method;
use url::Url;

use crate::api_structs::ResultFiles;

pub struct S3Handler {
    access_key: String,
    secret_key: String,
    bucket: String,
    endpoint: String,
    is_ssl: bool,
}

pub enum InputType {
    Fasta,
    Prodigal,
    RepliconsTSV,
}

impl InputType {
    pub fn to_str(&self) -> &str {
        match self {
            InputType::Fasta => "fastadata.fasta",
            InputType::Prodigal => "prodigal.tf",
            InputType::RepliconsTSV => "replicons.tsv",
        }
    }
}

impl S3Handler {
    pub fn new(access_key: String, secret_key: String, bucket: String, endpoint: String) -> Self {
        let ssl = if endpoint.starts_with("http://") {
            true
        } else {
            false
        };
        S3Handler {
            access_key,
            secret_key,
            bucket,
            endpoint,
            is_ssl: ssl,
        }
    }

    pub fn sign_upload_url(&self, job_id: &str, input_type: InputType) -> Result<String> {
        let key = format!("jobs/{job_id}/inputs/{}", input_type.to_str());
        sign_url(
            Method::PUT,
            &self.access_key,
            &self.secret_key,
            self.is_ssl,
            false,
            0,
            None,
            &self.bucket,
            &key,
            &self.endpoint,
            10000, // 10000 seconds = 2.77 hours should be enough for uploads
        )
    }
    pub fn sign_download_urls(&self, job_id: &str) -> Result<ResultFiles> {
        let get_download_url = |output_format: &str| -> Result<String> {
            let key = format!("jobs/{job_id}/results/result.{output_format}").to_string();
            sign_url(
                Method::GET,
                &self.access_key,
                &self.secret_key,
                self.is_ssl,
                false,
                0,
                None,
                &self.bucket,
                &key,
                &self.endpoint,
                60 * 86400, // 60 days
            )
        };
        Ok(ResultFiles {
            embl: get_download_url("embl")?,
            faa: get_download_url("faa")?,
            faa_hypothetical: get_download_url("hypotheticals.faa")?,
            ffn: get_download_url("ffn")?,
            fna: get_download_url("fna")?,
            gbff: get_download_url("gbff")?,
            gff3: get_download_url("gff")?,
            json: get_download_url("json")?,
            tsv: get_download_url("tsv")?,
            tsv_hypothetical: get_download_url("hypotheticals.tsv")?,
        })
    }
}

/// Creates a fully customized presigned S3 url.
///
/// ## Arguments:
///
/// * `method: http::Method` - Http method the request is valid for
/// * `access_key: &String` - Secret key id
/// * `secret_key: &String` - Secret key for access
/// * `ssl: bool` - Flag if the endpoint is accessible via ssl
/// * `multipart: bool` - Flag if the request is for a specific multipart part upload
/// * `part_number: i32` - Specific part number if multipart: true
/// * `upload_id: &String` - Multipart upload id if multipart: true
/// * `bucket: &String` - Bucket name
/// * `key: &String` - Full path of object in bucket
/// * `endpoint: &String` - Full path of object in bucket
/// * `duration: i64` - Full path of object in bucket
/// *
///
/// ## Returns:
///
/// * `` -
///
#[allow(clippy::too_many_arguments)]
fn sign_url(
    method: Method,
    access_key: &str,
    secret_key: &str,
    ssl: bool,
    multipart: bool,
    part_number: i32,
    upload_id: Option<String>,
    bucket: &str,
    key: &str,
    endpoint: &str,
    duration: i64,
) -> Result<String> {
    let signer = AwsV4Signer::new("s3", "RegionOne");

    // Set protocol depending if ssl
    let protocol = if ssl { "https://" } else { "http://" };

    // Remove http:// or https:// from beginning of endpoint url if present
    let endpoint_sanitized = if let Some(stripped) = endpoint.strip_prefix("https://") {
        stripped.to_string()
    } else if let Some(stripped) = endpoint.strip_prefix("http://") {
        stripped.to_string()
    } else {
        endpoint.to_string()
    };

    // Construct request
    let url = if multipart {
        let upload_id = upload_id
            .ok_or_else(|| anyhow!("No upload id provided for multipart presigned url"))?;
        Url::parse(&format!(
            "{}{}.{}/{}?partNumber={}&uploadId={}",
            protocol, bucket, endpoint_sanitized, key, part_number, upload_id
        ))?
    } else {
        Url::parse(&format!(
            "{}{}.{}/{}",
            protocol, bucket, endpoint_sanitized, key
        ))?
    };

    let mut req = reqwest::Request::new(method, url);

    // Signing request with Signer
    signer.sign_query(
        &mut req,
        std::time::Duration::new(duration as u64, 0), // Sec, nano
        &AwsCredential {
            access_key_id: access_key.to_string(),
            secret_access_key: secret_key.to_string(),
            session_token: None,
            expires_in: None,
        },
    )?;
    Ok(req.url().to_string())
}

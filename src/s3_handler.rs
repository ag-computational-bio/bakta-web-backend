use anyhow::{Result, anyhow};
use axum::http;
use reqsign::aws::{self, StaticCredentialProvider};
use reqwest::{Client, Method, StatusCode};
use url::Url;

use crate::{
    api_structs::ResultFiles,
    v2::api_structs::{
        BaktaProteinsResultFiles, BaktaResultFiles, BaktfoldResultFiles, ResultKind, UploadKind,
        V2ResultFiles,
    },
};

pub struct S3Handler {
    access_key: String,
    secret_key: String,
    bucket: String,
    endpoint: String,
    is_ssl: bool,
    client: Client,
}

const RESULT_DOWNLOAD_DURATION_SECONDS: i64 = 6 * 86400;
const RESULT_EXISTENCE_CHECK_DURATION_SECONDS: i64 = 60;

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

fn upload_object_name(kind: UploadKind) -> &'static str {
    match kind {
        UploadKind::GenomeFasta => "fastadata.fasta",
        UploadKind::ProdigalTrainingFile => "prodigal.tf",
        UploadKind::RepliconsTable => "replicons.tsv",
        UploadKind::RegionsFile => "regions",
        UploadKind::TrustedProteinsFile => "trusted_proteins.faa",
        UploadKind::HmmsFile => "trusted_hmms.hmm",
        UploadKind::ProteinFasta => "proteins.fasta",
        UploadKind::BaktaJson => "bakta.json",
    }
}

impl S3Handler {
    pub fn new(access_key: String, secret_key: String, bucket: String, endpoint: String) -> Self {
        let ssl = endpoint.starts_with("https://");
        S3Handler {
            access_key,
            secret_key,
            bucket,
            endpoint,
            is_ssl: ssl,
            client: Client::new(),
        }
    }

    pub async fn sign_upload_url(&self, job_id: &str, input_type: InputType) -> Result<String> {
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
            None,
        )
        .await
    }

    pub async fn sign_upload_url_v2(
        &self,
        job_id: &str,
        upload_kind: UploadKind,
    ) -> Result<String> {
        let key = format!("jobs/{job_id}/inputs/{}", upload_object_name(upload_kind));
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
            10000,
            None,
        )
        .await
    }

    async fn sign_result_object_url(
        &self,
        job_id: &str,
        key_name: &str,
        method: Method,
        duration: i64,
        disposition: Option<String>,
    ) -> Result<String> {
        let key = format!("jobs/{job_id}/results/{key_name}");
        sign_url(
            method,
            &self.access_key,
            &self.secret_key,
            self.is_ssl,
            false,
            0,
            None,
            &self.bucket,
            &key,
            &self.endpoint,
            duration,
            disposition,
        )
        .await
    }

    async fn sign_result_download_object(
        &self,
        job_id: &str,
        key_name: &str,
        download_name: String,
    ) -> Result<String> {
        self.sign_result_object_url(
            job_id,
            key_name,
            Method::GET,
            RESULT_DOWNLOAD_DURATION_SECONDS,
            Some(download_name),
        )
        .await
    }

    async fn result_object_exists(&self, job_id: &str, key_name: &str) -> Result<bool> {
        let url = self
            .sign_result_object_url(
                job_id,
                key_name,
                Method::HEAD,
                RESULT_EXISTENCE_CHECK_DURATION_SECONDS,
                None,
            )
            .await?;

        let response = self.client.head(url).send().await?;
        match response.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            status => Err(anyhow!(
                "Failed to check result file existence for {key_name}: {status}"
            )),
        }
    }

    async fn sign_result_download_url(
        &self,
        job_id: &str,
        name: &str,
        output_format: &str,
    ) -> Result<String> {
        self.sign_result_download_object(
            job_id,
            &format!("result.{output_format}"),
            format!("{name}.{output_format}"),
        )
        .await
    }

    async fn sign_optional_result_download_url(
        &self,
        job_id: &str,
        name: &str,
        output_format: &str,
    ) -> Result<Option<String>> {
        let key_name = format!("result.{output_format}");
        if !self.result_object_exists(job_id, &key_name).await? {
            return Ok(None);
        }

        self.sign_result_download_object(job_id, &key_name, format!("{name}.{output_format}"))
            .await
            .map(Some)
    }

    async fn sign_required_result_download_url(
        &self,
        job_id: &str,
        name: &str,
        output_format: &str,
    ) -> Result<String> {
        let key_name = format!("result.{output_format}");
        if !self.result_object_exists(job_id, &key_name).await? {
            return Err(anyhow!("Missing required result file: {key_name}"));
        }

        self.sign_result_download_object(job_id, &key_name, format!("{name}.{output_format}"))
            .await
    }

    pub async fn sign_download_urls(&self, job_id: &str, name: &str) -> Result<ResultFiles> {
        Ok(ResultFiles {
            embl: self.sign_result_download_url(job_id, name, "embl").await?,
            faa: self.sign_result_download_url(job_id, name, "faa").await?,
            faa_hypothetical: self
                .sign_result_download_url(job_id, name, "hypotheticals.faa")
                .await?,
            ffn: self.sign_result_download_url(job_id, name, "ffn").await?,
            fna: self.sign_result_download_url(job_id, name, "fna").await?,
            gbff: self.sign_result_download_url(job_id, name, "gbff").await?,
            gff3: self.sign_result_download_url(job_id, name, "gff3").await?,
            json: self.sign_result_download_url(job_id, name, "json").await?,
            tsv: self.sign_result_download_url(job_id, name, "tsv").await?,
            tsv_hypothetical: self
                .sign_result_download_url(job_id, name, "hypotheticals.tsv")
                .await?,
            tsv_inference: self
                .sign_result_download_url(job_id, name, "inference.tsv")
                .await?,
            txt_logs: self.sign_result_download_url(job_id, name, "txt").await?,
            png_circular_plot: self.sign_result_download_url(job_id, name, "png").await?,
            svg_circular_plot: self.sign_result_download_url(job_id, name, "svg").await?,
        })
    }

    pub async fn sign_download_urls_v2(
        &self,
        job_id: &str,
        name: &str,
        result_kind: ResultKind,
    ) -> Result<V2ResultFiles> {
        Ok(match result_kind {
            ResultKind::Bakta => V2ResultFiles::Bakta(BaktaResultFiles {
                embl: self
                    .sign_optional_result_download_url(job_id, name, "embl")
                    .await?,
                faa: self
                    .sign_optional_result_download_url(job_id, name, "faa")
                    .await?,
                hypotheticals_faa: self
                    .sign_optional_result_download_url(job_id, name, "hypotheticals.faa")
                    .await?,
                ffn: self
                    .sign_optional_result_download_url(job_id, name, "ffn")
                    .await?,
                fna: self
                    .sign_optional_result_download_url(job_id, name, "fna")
                    .await?,
                gbff: self
                    .sign_optional_result_download_url(job_id, name, "gbff")
                    .await?,
                gff3: self
                    .sign_optional_result_download_url(job_id, name, "gff3")
                    .await?,
                json: self
                    .sign_required_result_download_url(job_id, name, "json")
                    .await?,
                tsv: self
                    .sign_optional_result_download_url(job_id, name, "tsv")
                    .await?,
                hypotheticals_tsv: self
                    .sign_optional_result_download_url(job_id, name, "hypotheticals.tsv")
                    .await?,
                logs_txt: self
                    .sign_optional_result_download_url(job_id, name, "txt")
                    .await?,
                inference_tsv: self
                    .sign_optional_result_download_url(job_id, name, "inference.tsv")
                    .await?,
                circular_plot_png: self
                    .sign_optional_result_download_url(job_id, name, "png")
                    .await?,
                circular_plot_svg: self
                    .sign_optional_result_download_url(job_id, name, "svg")
                    .await?,
            }),
            ResultKind::BaktaProteins => V2ResultFiles::BaktaProteins(BaktaProteinsResultFiles {
                tsv: self
                    .sign_optional_result_download_url(job_id, name, "tsv")
                    .await?,
                faa: self
                    .sign_optional_result_download_url(job_id, name, "faa")
                    .await?,
                hypotheticals_tsv: self
                    .sign_optional_result_download_url(job_id, name, "hypotheticals.tsv")
                    .await?,
                json: self
                    .sign_required_result_download_url(job_id, name, "json")
                    .await?,
            }),
            ResultKind::Baktfold => V2ResultFiles::Baktfold(BaktfoldResultFiles {
                embl: self
                    .sign_optional_result_download_url(job_id, name, "embl")
                    .await?,
                faa: self
                    .sign_optional_result_download_url(job_id, name, "faa")
                    .await?,
                hypotheticals_faa: self
                    .sign_optional_result_download_url(job_id, name, "hypotheticals.faa")
                    .await?,
                ffn: self
                    .sign_optional_result_download_url(job_id, name, "ffn")
                    .await?,
                fna: self
                    .sign_optional_result_download_url(job_id, name, "fna")
                    .await?,
                gbff: self
                    .sign_optional_result_download_url(job_id, name, "gbff")
                    .await?,
                gff3: self
                    .sign_optional_result_download_url(job_id, name, "gff3")
                    .await?,
                json: self
                    .sign_required_result_download_url(job_id, name, "json")
                    .await?,
                tsv: self
                    .sign_optional_result_download_url(job_id, name, "tsv")
                    .await?,
                hypotheticals_tsv: self
                    .sign_optional_result_download_url(job_id, name, "hypotheticals.tsv")
                    .await?,
                logs_txt: self
                    .sign_optional_result_download_url(job_id, name, "txt")
                    .await?,
                inference_tsv: self
                    .sign_optional_result_download_url(job_id, name, "inference.tsv")
                    .await?,
            }),
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
async fn sign_url(
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
    disposition: Option<String>,
) -> Result<String> {
    let signer = aws::default_signer("s3", "RegionOne")
        .with_credential_provider(StaticCredentialProvider::new(access_key, secret_key));

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
    } else if disposition.is_some() && method == Method::GET {
        let url_encoded_disposition = url::form_urlencoded::byte_serialize(
            format!(r#"attachment; filename="{}""#, disposition.unwrap()).as_bytes(),
        )
        .collect::<String>();
        Url::parse(&format!(
            "{}{}.{}/{}?response-content-disposition={}",
            protocol, bucket, endpoint_sanitized, key, url_encoded_disposition
        ))?
    } else {
        Url::parse(&format!(
            "{}{}.{}/{}",
            protocol, bucket, endpoint_sanitized, key
        ))?
    };

    let mut parts = http::Request::builder()
        .method(method)
        .uri(url.as_str())
        .body(())?
        .into_parts()
        .0;

    // Signing request with Signer
    signer
        .sign(
            &mut parts,
            Some(std::time::Duration::new(duration as u64, 0)), // Sec, nano
        )
        .await?;
    Ok(parts.uri.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upload_object_name_mapping() {
        assert_eq!(
            upload_object_name(UploadKind::GenomeFasta),
            "fastadata.fasta"
        );
        assert_eq!(
            upload_object_name(UploadKind::TrustedProteinsFile),
            "trusted_proteins.faa"
        );
        assert_eq!(upload_object_name(UploadKind::BaktaJson), "bakta.json");
    }
}

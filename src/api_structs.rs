use crate::api_paths::*;
use anyhow::anyhow;
use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, OpenApi, ToSchema};
use uuid::Uuid;

#[derive(OpenApi)]
#[openapi(
    paths(delete_job, init_job, list_jobs, query_result, start_job, version),
    components(schemas(
        Job,
        InitRequest,
        InitResponse,
        RepliconTableType,
        ListRequest,
        ListResponse,
        JobStatusEnum,
        JobStatus,
        FailedJobStatus,
        FailedJobStatusEnum,
        ResultFiles,
        ResultResponse,
        StartRequest,
        JobConfig,
        DermType,
        VersionResponse,
    ))
)]
pub struct BaktaApi;

#[derive(ToSchema, Serialize, Deserialize, IntoParams)]
pub struct Job {
    pub secret: String,
    #[serde(rename = "jobID")]
    pub id: Uuid,
}

#[derive(ToSchema, Serialize, Deserialize, Default)]
pub enum RepliconTableType {
    #[default]
    CSV,
    TSV,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct InitRequest {
    pub name: String,
    #[serde(rename = "repliconTableType")]
    pub replicon_type: RepliconTableType,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct InitResponse {
    #[serde(rename = "uploadLinkFasta")]
    pub fasta_url: String,
    #[serde(rename = "uploadLinkProdigal")]
    pub prodigal_url: String,
    #[serde(rename = "uploadLinkReplicons")]
    pub replicon_url: String,
    pub job: Job,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct ListRequest {
    pub jobs: Vec<Job>,
}

#[derive(ToSchema, Serialize, Deserialize, Clone)]
pub enum JobStatusEnum {
    INIT,
    RUNNING,
    SUCCESSFULL,
    ERROR,
}

impl TryFrom<String> for JobStatusEnum {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        match value.as_str() {
            "Init" | "Pending" => Ok(JobStatusEnum::INIT),
            "Running" => Ok(JobStatusEnum::RUNNING),
            "Succeeded" => Ok(JobStatusEnum::SUCCESSFULL),
            "Failed" | "Error" => Ok(JobStatusEnum::ERROR),
            _ => Err(anyhow!("Invalid JobStatus")),
        }
    }
}

#[derive(ToSchema, Serialize, Deserialize)]
pub enum FailedJobStatusEnum {
    #[serde(rename = "NOT_FOUND")]
    NotFound,
    #[serde(rename = "UNAUTHORIZED")]
    Unauthorized,
}

#[derive(ToSchema, Serialize, Deserialize, Clone)]
pub struct JobStatus {
    #[serde(rename = "jobID")]
    pub id: Uuid,
    #[serde(rename = "jobStatus")]
    pub status: JobStatusEnum,
    pub started: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub name: String,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct FailedJobStatus {
    #[serde(rename = "jobID")]
    pub id: Uuid,
    #[serde(rename = "jobStatus")]
    pub status: FailedJobStatusEnum,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct ListResponse {
    pub jobs: Vec<JobStatus>,
    #[serde(rename = "failedJobs")]
    pub failed: Vec<FailedJobStatus>,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct ResultFiles {
    #[serde(rename = "EMBL")]
    pub embl: String,
    #[serde(rename = "FAA")]
    pub faa: String,
    #[serde(rename = "FAAHypothetical")]
    pub faa_hypothetical: String,
    #[serde(rename = "FFN")]
    pub ffn: String,
    #[serde(rename = "FNA")]
    pub fna: String,
    #[serde(rename = "GBFF")]
    pub gbff: String,
    #[serde(rename = "GFF3")]
    pub gff3: String,
    #[serde(rename = "JSON")]
    pub json: String,
    #[serde(rename = "TSV")]
    pub tsv: String,
    #[serde(rename = "TSVHypothetical")]
    pub tsv_hypothetical: String,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct ResultResponse {
    #[serde(rename = "jobID")]
    pub id: Uuid,
    pub started: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub name: String,
    #[serde(rename = "ResultFiles")]
    pub files: ResultFiles,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub enum DermType {
    UNKNOWN,
    MONODERM,
    DIDERM,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct JobConfig {
    #[serde(rename = "hasProdigal")]
    pub prodigal: Option<bool>,
    #[serde(rename = "hasReplicons")]
    pub replicons: Option<bool>,
    #[serde(rename = "translationTable")]
    pub table: Option<u8>,
    #[serde(rename = "completeGenome")]
    pub complete: Option<bool>,
    #[serde(rename = "keepContigHeaders")]
    pub headers: Option<bool>,
    #[serde(rename = "minContigLength")]
    pub min_length: Option<String>,
    #[serde(rename = "dermType")]
    pub derm: Option<DermType>,
    pub genus: Option<String>,
    pub species: Option<String>,
    pub strain: Option<String>,
    pub plasmid: Option<String>,
    pub locus: Option<String>,
    #[serde(rename = "locusTag")]
    pub locus_tag: Option<String>,
    pub compliant: Option<bool>,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct StartRequest {
    pub job: Job,
    pub config: JobConfig,
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct VersionResponse {
    #[serde(rename = "toolVersion")]
    pub tool: String,
    #[serde(rename = "dbVersion")]
    pub db: String,
    #[serde(rename = "backendVersion")]
    pub backend: String,
}

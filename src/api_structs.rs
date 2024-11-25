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
    info(title = "Bakta Web API"),
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

#[derive(ToSchema, Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

fn default_replicons() -> bool {
    true
}

#[derive(ToSchema, Serialize, Deserialize, Default)]
pub struct JobConfig {
    #[serde(rename = "prodigalTrainingFile")]
    pub prodigal: Option<String>,
    #[serde(rename = "hasReplicons", default = "default_replicons")]
    pub replicons: bool,
    #[serde(rename = "translationTable")]
    pub table: u8,
    #[serde(rename = "completeGenome")]
    pub complete: bool,
    #[serde(rename = "keepContigHeaders")]
    pub headers: bool,
    #[serde(rename = "minContigLength")]
    pub min_length: u64,
    #[serde(rename = "dermType")]
    pub derm: Option<DermType>,
    pub genus: Option<String>,
    pub species: Option<String>,
    pub strain: Option<String>,
    pub plasmid: Option<String>,
    pub locus: Option<String>,
    #[serde(rename = "locusTag")]
    pub locus_tag: Option<String>,
    pub compliant: bool,
}

impl JobConfig {
    pub fn into_parameters(self) -> String {
        let mut parameters = Vec::new();

        if self.min_length > 1 {
            parameters.push(format!("--min-contig-length {}", self.min_length));
        }

        if self.prodigal.is_some() {
            parameters.push("--prodigal /data/prodigal.tf".to_string());
        }

        if self.replicons {
            parameters.push("--replicons /data/replicons.tsv".to_string());
        }

        if self.complete {
            parameters.push("--complete".to_string());
        }

        if let Some(locus) = self.locus {
            if !locus.is_empty() {
                parameters.push(format!("--locus {}", locus));
            }
        }

        if let Some(locus_tag) = self.locus_tag {
            if !locus_tag.is_empty() {
                parameters.push(format!("--locus-tag {}", locus_tag));
            }
        }

        if self.headers {
            parameters.push("--keep-contig-headers".to_string());
        }

        if let Some(genus) = self.genus {
            if !genus.is_empty() {
                parameters.push(format!("--genus {}", genus));
            }
        }

        if let Some(species) = self.species {
            if !species.is_empty() {
                parameters.push(format!("--species {}", species));
            }
        }

        if let Some(strain) = self.strain {
            if !strain.is_empty() {
                parameters.push(format!("--strain {}", strain));
            }
        }

        if let Some(plasmid) = self.plasmid {
            if !plasmid.is_empty() {
                parameters.push(format!("--plasmid {}", plasmid));
            }
        }

        if self.compliant {
            parameters.push("--compliant".to_string());
        }

        if let 4 = self.table {
            parameters.push("--translation-table 4".to_string());
        }

        match self.derm {
            Some(DermType::MONODERM) => parameters.push("--gram +".to_string()),
            Some(DermType::DIDERM) => parameters.push("--gram -".to_string()),
            _ => parameters.push("--gram ?".to_string()),
        }

        parameters.join(" ")
    }
}

#[derive(ToSchema, Serialize, Deserialize)]
pub struct StartRequest {
    pub job: Job,
    pub config: JobConfig,
}

#[derive(ToSchema, Serialize, Deserialize, Clone)]
pub struct VersionResponse {
    #[serde(rename = "toolVersion")]
    pub tool: String,
    #[serde(rename = "dbVersion")]
    pub db: String,
    #[serde(rename = "backendVersion")]
    pub backend: String,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_into_parameters() {
        let params = JobConfig {
            prodigal: Some("foo".to_string()),
            replicons: true,
            table: 4,
            complete: true,
            headers: true,
            min_length: 1,
            derm: Some(DermType::MONODERM),
            genus: Some("Bacillus".to_string()),
            species: Some("subtilis".to_string()),
            strain: Some("168".to_string()),
            plasmid: Some("pBS32".to_string()),
            locus: Some("BSU_00010".to_string()),
            locus_tag: Some("BSU00010".to_string()),
            compliant: true,
        };

        assert_eq!(
            params.into_parameters(),
            "--prodigal /data/prodigal.tf --replicons /data/replicons.tsv --complete --locus BSU_00010 --locus-tag BSU00010 --keep-contig-headers --genus Bacillus --species subtilis --strain 168 --plasmid pBS32 --compliant --translation-table 4 --gram +"
        );

        assert_eq!(JobConfig::default().into_parameters(), "--derm ?");
    }
}

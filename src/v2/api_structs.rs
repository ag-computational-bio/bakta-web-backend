#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, OpenApi, ToSchema};
use uuid::Uuid;

use crate::v2::api_paths::*;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Bakta Web API V2",
        description = "Version 2 API for scheduling Bakta, Bakta Proteins, Baktfold, and combined workflows.",
        license(name = "MIT", url = "https://opensource.org/license/mit/")
    ),
    paths(
        workflows,
        init_job,
        list_jobs,
        query_result,
        start_job,
        job_logs,
        delete_job,
        version
    ),
    components(schemas(
        WorkflowKind,
        ResultKind,
        UploadKind,
        DermType,
        StageStatus,
        JobStatus,
        FailedJobStatusKind,
        JobReference,
        FailedJobStatus,
        UploadDescriptor,
        UploadLink,
        WorkflowDescriptorResponse,
        V2InitRequest,
        V2InitResponse,
        EmptyConfig,
        BaktaV2Config,
        V2StartRequest,
        WorkflowStartConfig,
        V2ListRequest,
        V2JobStatus,
        V2ListResponse,
        BaktaResultFiles,
        BaktaProteinsResultFiles,
        BaktfoldResultFiles,
        V2ResultFiles,
        V2ResultResponse,
        StageLog,
        V2LogsResponse,
        V2VersionResponse,
    ))
)]
pub struct BaktaApiV2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowKind {
    Bakta,
    BaktaProteins,
    BaktaBaktfold,
    Baktfold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    Bakta,
    BaktaProteins,
    Baktfold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UploadKind {
    GenomeFasta,
    ProdigalTrainingFile,
    RepliconsTable,
    RegionsFile,
    TrustedProteinsFile,
    HmmsFile,
    ProteinFasta,
    BaktaJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DermType {
    Unknown,
    Monoderm,
    Diderm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Init,
    Running,
    Successful,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailedJobStatusKind {
    NotFound,
    Unauthorized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, IntoParams)]
#[into_params(style = Form, parameter_in = Query)]
pub struct JobReference {
    pub secret: String,
    pub job_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FailedJobStatus {
    pub job_id: Uuid,
    pub status: FailedJobStatusKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadDescriptor {
    pub upload_kind: UploadKind,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadLink {
    pub upload_kind: UploadKind,
    pub required: bool,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkflowDescriptorResponse {
    pub workflow_kind: WorkflowKind,
    pub result_kind: ResultKind,
    pub uploads: Vec<UploadDescriptor>,
    pub stages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2InitRequest {
    pub name: String,
    pub workflow_kind: WorkflowKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2InitResponse {
    pub job: JobReference,
    pub workflow_kind: WorkflowKind,
    pub uploads: Vec<UploadLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
pub struct EmptyConfig {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BaktaV2Config {
    pub use_prodigal_training_file: bool,
    pub use_replicons: bool,
    pub use_regions: bool,
    pub use_trusted_proteins: bool,
    pub use_hmms: bool,
    pub translation_table: u8,
    pub complete_genome: bool,
    pub keep_contig_headers: bool,
    pub min_contig_length: u64,
    pub derm_type: Option<DermType>,
    pub genus: Option<String>,
    pub species: Option<String>,
    pub strain: Option<String>,
    pub plasmid: Option<String>,
    pub locus: Option<String>,
    pub locus_tag: Option<String>,
    pub locus_tag_increment: u8,
    pub compliant: bool,
    pub meta: bool,
    pub skip_trna: bool,
    pub skip_tmrna: bool,
    pub skip_rrna: bool,
    pub skip_ncrna: bool,
    pub skip_ncrna_region: bool,
    pub skip_crispr: bool,
    pub skip_cds: bool,
    pub skip_pseudo: bool,
    pub skip_sorf: bool,
    pub skip_gap: bool,
    pub skip_ori: bool,
    pub skip_filter: bool,
    pub skip_plot: bool,
}

impl Default for BaktaV2Config {
    fn default() -> Self {
        Self {
            use_prodigal_training_file: false,
            use_replicons: false,
            use_regions: false,
            use_trusted_proteins: false,
            use_hmms: false,
            translation_table: 11,
            complete_genome: false,
            keep_contig_headers: false,
            min_contig_length: 0,
            derm_type: None,
            genus: None,
            species: None,
            strain: None,
            plasmid: None,
            locus: None,
            locus_tag: None,
            locus_tag_increment: 1,
            compliant: false,
            meta: false,
            skip_trna: false,
            skip_tmrna: false,
            skip_rrna: false,
            skip_ncrna: false,
            skip_ncrna_region: false,
            skip_crispr: false,
            skip_cds: false,
            skip_pseudo: false,
            skip_sorf: false,
            skip_gap: false,
            skip_ori: false,
            skip_filter: false,
            skip_plot: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2StartRequest {
    pub job: JobReference,
    #[serde(flatten)]
    pub workflow: WorkflowStartConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "workflow_kind", content = "config", rename_all = "snake_case")]
pub enum WorkflowStartConfig {
    Bakta(BaktaV2Config),
    BaktaProteins(EmptyConfig),
    BaktaBaktfold(BaktaV2Config),
    Baktfold(EmptyConfig),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2ListRequest {
    pub jobs: Vec<JobReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2JobStatus {
    pub job_id: Uuid,
    pub status: JobStatus,
    pub workflow_kind: WorkflowKind,
    pub result_kind: ResultKind,
    pub started: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2ListResponse {
    pub jobs: Vec<V2JobStatus>,
    pub failed_jobs: Vec<FailedJobStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BaktaResultFiles {
    pub embl: String,
    pub faa: String,
    pub hypotheticals_faa: String,
    pub ffn: String,
    pub fna: String,
    pub gbff: String,
    pub gff3: String,
    pub json: String,
    pub tsv: String,
    pub hypotheticals_tsv: String,
    pub logs_txt: String,
    pub inference_tsv: String,
    pub circular_plot_png: String,
    pub circular_plot_svg: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BaktaProteinsResultFiles {
    pub tsv: String,
    pub faa: String,
    pub hypotheticals_tsv: String,
    pub json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BaktfoldResultFiles {
    pub embl: String,
    pub faa: String,
    pub hypotheticals_faa: String,
    pub ffn: String,
    pub fna: String,
    pub gbff: String,
    pub gff3: String,
    pub json: String,
    pub tsv: String,
    pub hypotheticals_tsv: String,
    pub logs_txt: String,
    pub inference_tsv: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "result_kind", content = "files", rename_all = "snake_case")]
pub enum V2ResultFiles {
    Bakta(BaktaResultFiles),
    BaktaProteins(BaktaProteinsResultFiles),
    Baktfold(BaktfoldResultFiles),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2ResultResponse {
    pub job_id: Uuid,
    pub workflow_kind: WorkflowKind,
    pub started: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub name: String,
    pub result: V2ResultFiles,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StageLog {
    pub stage: String,
    pub status: StageStatus,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2LogsResponse {
    pub workflow_kind: WorkflowKind,
    pub stages: Vec<StageLog>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2VersionResponse {
    pub backend_version: String,
    pub bakta_version: String,
    pub bakta_db_version: String,
    pub baktfold_version: String,
    pub baktfold_db_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bakta_v2_config_defaults() {
        let config = BaktaV2Config::default();
        assert_eq!(config.translation_table, 11);
        assert_eq!(config.locus_tag_increment, 1);
        assert!(!config.use_regions);
        assert!(!config.meta);
    }
}

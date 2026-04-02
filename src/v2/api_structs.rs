#![allow(dead_code)]

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, OpenApi, ToSchema};
use uuid::Uuid;

use crate::v2::api_paths::*;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Bakta Web API V2",
        description = "Extensible V2 API for scheduling multiple workflow kinds, including Bakta, Bakta Proteins, Baktfold, and combined Bakta plus Baktfold jobs.",
        license(name = "MIT", url = "https://opensource.org/license/mit/")
    ),
    tags(
        (name = "v2", description = "V2 workflow scheduling API.")
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

/// Workflow kind exposed by the V2 API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowKind {
    Bakta,
    BaktaProteins,
    BaktaBaktfold,
    Baktfold,
}

/// Result family returned by a finished V2 workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    Bakta,
    BaktaProteins,
    Baktfold,
}

/// Upload slot that a V2 workflow may require before it can be started.
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

/// Shared V2 job handle used for authenticated job operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, IntoParams)]
#[into_params(style = Form, parameter_in = Query)]
pub struct JobReference {
    /// Secret returned during job initialization and required for later job operations.
    pub secret: String,
    /// Unique job identifier returned during job initialization.
    pub job_id: Uuid,
}

/// Failure details for requested jobs that could not be listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FailedJobStatus {
    pub job_id: Uuid,
    pub status: FailedJobStatusKind,
}

/// Description of a single upload slot for a workflow kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadDescriptor {
    pub upload_kind: UploadKind,
    pub required: bool,
}

/// Presigned upload URL for a workflow-specific input slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadLink {
    pub upload_kind: UploadKind,
    pub required: bool,
    pub url: String,
}

/// Static workflow metadata returned by `GET /api/v2/workflows`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkflowDescriptorResponse {
    pub workflow_kind: WorkflowKind,
    pub result_kind: ResultKind,
    pub uploads: Vec<UploadDescriptor>,
    pub stages: Vec<String>,
}

/// Request body for creating a new V2 job and receiving workflow-specific upload URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2InitRequest {
    pub name: String,
    pub workflow_kind: WorkflowKind,
}

/// Upload URLs and job credentials returned by the V2 initialization endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2InitResponse {
    pub job: JobReference,
    pub workflow_kind: WorkflowKind,
    pub uploads: Vec<UploadLink>,
}

/// Empty configuration payload used for workflow kinds that currently expose no extra options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
pub struct EmptyConfig {}

fn sanitize_input(s: String) -> String {
    format!("'{}'", s.replace('\'', ""))
}

/// V2 Bakta configuration surface.
///
/// This is intentionally broader than V1 and includes optional upload-backed flags as booleans.
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

impl BaktaV2Config {
    pub fn into_parameters(self) -> Result<String> {
        let mut parameters = Vec::new();

        if self.min_contig_length > 1 {
            parameters.push(format!("--min-contig-length {}", self.min_contig_length));
        }

        if self.use_prodigal_training_file {
            parameters.push("--prodigal /data/prodigal.tf".to_string());
        }

        if self.use_replicons {
            parameters.push("--replicons /data/replicons.tsv".to_string());
        }

        if self.use_regions {
            parameters.push("--regions /data/regions".to_string());
        }

        if self.use_trusted_proteins {
            parameters.push("--proteins /data/trusted_proteins.faa".to_string());
        }

        if self.use_hmms {
            parameters.push("--hmms /data/trusted_hmms.hmm".to_string());
        }

        if self.complete_genome {
            parameters.push("--complete".to_string());
        }

        if let Some(locus) = self.locus
            && !locus.is_empty()
        {
            parameters.push(format!("--locus {}", sanitize_input(locus)));
        }

        if let Some(locus_tag) = self.locus_tag
            && !locus_tag.is_empty()
        {
            parameters.push(format!("--locus-tag {}", sanitize_input(locus_tag)));
        }

        if !matches!(self.locus_tag_increment, 1 | 5 | 10) {
            return Err(anyhow!("Invalid locus_tag_increment"));
        }
        if self.locus_tag_increment != 1 {
            parameters.push(format!(
                "--locus-tag-increment {}",
                self.locus_tag_increment
            ));
        }

        if self.keep_contig_headers {
            parameters.push("--keep-contig-headers".to_string());
        }

        if let Some(genus) = self.genus
            && !genus.is_empty()
        {
            parameters.push(format!("--genus {}", sanitize_input(genus)));
        }

        if let Some(species) = self.species
            && !species.is_empty()
        {
            parameters.push(format!("--species {}", sanitize_input(species)));
        }

        if let Some(strain) = self.strain
            && !strain.is_empty()
        {
            parameters.push(format!("--strain {}", sanitize_input(strain)));
        }

        if let Some(plasmid) = self.plasmid
            && !plasmid.is_empty()
        {
            parameters.push(format!("--plasmid {}", sanitize_input(plasmid)));
        }

        if self.compliant {
            parameters.push("--compliant".to_string());
        }

        if self.meta {
            parameters.push("--meta".to_string());
        }

        match self.translation_table {
            11 => {}
            4 | 25 => parameters.push(format!("--translation-table {}", self.translation_table)),
            _ => return Err(anyhow!("Invalid translation_table")),
        }

        match self.derm_type {
            Some(DermType::Monoderm) => parameters.push("--gram +".to_string()),
            Some(DermType::Diderm) => parameters.push("--gram -".to_string()),
            _ => parameters.push("--gram ?".to_string()),
        }

        if self.skip_trna {
            parameters.push("--skip-trna".to_string());
        }
        if self.skip_tmrna {
            parameters.push("--skip-tmrna".to_string());
        }
        if self.skip_rrna {
            parameters.push("--skip-rrna".to_string());
        }
        if self.skip_ncrna {
            parameters.push("--skip-ncrna".to_string());
        }
        if self.skip_ncrna_region {
            parameters.push("--skip-ncrna-region".to_string());
        }
        if self.skip_crispr {
            parameters.push("--skip-crispr".to_string());
        }
        if self.skip_cds {
            parameters.push("--skip-cds".to_string());
        }
        if self.skip_pseudo {
            parameters.push("--skip-pseudo".to_string());
        }
        if self.skip_sorf {
            parameters.push("--skip-sorf".to_string());
        }
        if self.skip_gap {
            parameters.push("--skip-gap".to_string());
        }
        if self.skip_ori {
            parameters.push("--skip-ori".to_string());
        }
        if self.skip_filter {
            parameters.push("--skip-filter".to_string());
        }
        if self.skip_plot {
            parameters.push("--skip-plot".to_string());
        }

        Ok(parameters.join(" "))
    }
}

/// Request body for starting a V2 workflow after all required uploads are complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2StartRequest {
    pub job: JobReference,
    #[serde(flatten)]
    pub workflow: WorkflowStartConfig,
}

/// Workflow-specific configuration payload accepted by the V2 start endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "workflow_kind", content = "config", rename_all = "snake_case")]
pub enum WorkflowStartConfig {
    Bakta(BaktaV2Config),
    BaktaProteins(EmptyConfig),
    BaktaBaktfold(BaktaV2Config),
    Baktfold(EmptyConfig),
}

impl WorkflowStartConfig {
    pub fn workflow_kind(&self) -> WorkflowKind {
        match self {
            WorkflowStartConfig::Bakta(_) => WorkflowKind::Bakta,
            WorkflowStartConfig::BaktaProteins(_) => WorkflowKind::BaktaProteins,
            WorkflowStartConfig::BaktaBaktfold(_) => WorkflowKind::BaktaBaktfold,
            WorkflowStartConfig::Baktfold(_) => WorkflowKind::Baktfold,
        }
    }

    pub fn into_parameters(self) -> Result<String> {
        match self {
            WorkflowStartConfig::Bakta(config) | WorkflowStartConfig::BaktaBaktfold(config) => {
                config.into_parameters()
            }
            WorkflowStartConfig::BaktaProteins(_) | WorkflowStartConfig::Baktfold(_) => {
                Ok(String::new())
            }
        }
    }
}

/// Request body for listing multiple V2 jobs at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2ListRequest {
    pub jobs: Vec<JobReference>,
}

/// Status entry returned for a single V2 job.
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

/// V2 response body for listing job states in bulk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2ListResponse {
    pub jobs: Vec<V2JobStatus>,
    pub failed_jobs: Vec<FailedJobStatus>,
}

/// Bakta result artifact set exposed by V2.
///
/// The `json` artifact is required. All other artifacts are omitted when the
/// workflow did not produce them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BaktaResultFiles {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faa: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypotheticals_faa: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ffn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fna: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gbff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gff3: Option<String>,
    pub json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tsv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypotheticals_tsv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs_txt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_tsv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circular_plot_png: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub circular_plot_svg: Option<String>,
}

/// Bakta Proteins result artifact set exposed by V2.
///
/// The `json` artifact is required. All other artifacts are omitted when the
/// workflow did not produce them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BaktaProteinsResultFiles {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tsv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faa: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypotheticals_tsv: Option<String>,
    pub json: String,
}

/// Baktfold result artifact set exposed by V2.
///
/// The `json` artifact is required. All other artifacts are omitted when the
/// workflow did not produce them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BaktfoldResultFiles {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faa: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypotheticals_faa: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ffn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fna: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gbff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gff3: Option<String>,
    pub json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tsv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hypotheticals_tsv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs_txt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_tsv: Option<String>,
}

/// Tagged V2 result payload for finished workflows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "result_kind", content = "files", rename_all = "snake_case")]
pub enum V2ResultFiles {
    Bakta(BaktaResultFiles),
    BaktaProteins(BaktaProteinsResultFiles),
    Baktfold(BaktfoldResultFiles),
}

/// V2 response body for a finished workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2ResultResponse {
    pub job_id: Uuid,
    pub workflow_kind: WorkflowKind,
    pub started: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub name: String,
    pub result: V2ResultFiles,
}

/// Stage-specific log output returned by the V2 logs endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StageLog {
    pub stage: String,
    pub status: StageStatus,
    pub content: String,
}

/// Structured logs for a V2 workflow, grouped by logical stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct V2LogsResponse {
    pub workflow_kind: WorkflowKind,
    pub stages: Vec<StageLog>,
}

/// Tool and database version information exposed by V2.
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

    #[test]
    fn test_bakta_v2_config_into_parameters() {
        let params = BaktaV2Config {
            use_prodigal_training_file: true,
            use_replicons: true,
            use_regions: true,
            use_trusted_proteins: true,
            use_hmms: true,
            translation_table: 25,
            complete_genome: true,
            keep_contig_headers: true,
            min_contig_length: 200,
            derm_type: Some(DermType::Monoderm),
            genus: Some("Bacillus".to_string()),
            species: Some("subtilis".to_string()),
            strain: Some("168".to_string()),
            plasmid: Some("pBS32".to_string()),
            locus: Some("BSU_00010".to_string()),
            locus_tag: Some("BSU00010".to_string()),
            locus_tag_increment: 5,
            compliant: true,
            meta: true,
            skip_trna: true,
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
            skip_plot: true,
        };

        assert_eq!(
            params.into_parameters().expect("parameters should build"),
            "--min-contig-length 200 --prodigal /data/prodigal.tf --replicons /data/replicons.tsv --regions /data/regions --proteins /data/trusted_proteins.faa --hmms /data/trusted_hmms.hmm --complete --locus 'BSU_00010' --locus-tag 'BSU00010' --locus-tag-increment 5 --keep-contig-headers --genus 'Bacillus' --species 'subtilis' --strain '168' --plasmid 'pBS32' --compliant --meta --translation-table 25 --gram + --skip-trna --skip-plot"
        );
    }

    #[test]
    fn test_bakta_v2_config_rejects_invalid_translation_table() {
        let error = BaktaV2Config {
            translation_table: 12,
            ..Default::default()
        }
        .into_parameters()
        .expect_err("invalid translation table should be rejected");

        assert_eq!(error.to_string(), "Invalid translation_table");
    }

    #[test]
    fn test_bakta_v2_config_rejects_invalid_locus_tag_increment() {
        let error = BaktaV2Config {
            locus_tag_increment: 3,
            ..Default::default()
        }
        .into_parameters()
        .expect_err("invalid locus tag increment should be rejected");

        assert_eq!(error.to_string(), "Invalid locus_tag_increment");
    }

    #[test]
    fn test_bakta_v2_config_sanitizes_embedded_quotes() {
        let params = BaktaV2Config {
            genus: Some("Bacil'lus".to_string()),
            strain: Some("O'Brien isolate".to_string()),
            ..Default::default()
        }
        .into_parameters()
        .expect("parameters should build");

        assert!(params.contains("--genus 'Bacillus'"));
        assert!(params.contains("--strain 'OBrien isolate'"));
        assert!(!params.contains("Bacil'lus"));
        assert!(!params.contains("O'Brien"));
    }

    #[test]
    fn test_bakta_result_files_skip_missing_optional_urls() {
        let value = serde_json::to_value(BaktaResultFiles {
            embl: None,
            faa: Some("faa-url".to_string()),
            hypotheticals_faa: None,
            ffn: None,
            fna: None,
            gbff: None,
            gff3: None,
            json: "json-url".to_string(),
            tsv: None,
            hypotheticals_tsv: None,
            logs_txt: None,
            inference_tsv: None,
            circular_plot_png: None,
            circular_plot_svg: None,
        })
        .expect("serialization should succeed");

        assert_eq!(value.get("json").and_then(|v| v.as_str()), Some("json-url"));
        assert_eq!(value.get("faa").and_then(|v| v.as_str()), Some("faa-url"));
        assert!(value.get("embl").is_none());
        assert!(value.get("circular_plot_png").is_none());
    }

    #[test]
    fn test_bakta_proteins_result_files_skip_missing_optional_urls() {
        let value = serde_json::to_value(BaktaProteinsResultFiles {
            tsv: None,
            faa: None,
            hypotheticals_tsv: Some("hypotheticals-url".to_string()),
            json: "json-url".to_string(),
        })
        .expect("serialization should succeed");

        assert_eq!(value.get("json").and_then(|v| v.as_str()), Some("json-url"));
        assert_eq!(
            value.get("hypotheticals_tsv").and_then(|v| v.as_str()),
            Some("hypotheticals-url")
        );
        assert!(value.get("tsv").is_none());
        assert!(value.get("faa").is_none());
    }

    #[test]
    fn test_baktfold_result_files_skip_missing_optional_urls() {
        let value = serde_json::to_value(BaktfoldResultFiles {
            embl: None,
            faa: None,
            hypotheticals_faa: None,
            ffn: None,
            fna: None,
            gbff: None,
            gff3: Some("gff3-url".to_string()),
            json: "json-url".to_string(),
            tsv: None,
            hypotheticals_tsv: None,
            logs_txt: None,
            inference_tsv: None,
        })
        .expect("serialization should succeed");

        assert_eq!(value.get("json").and_then(|v| v.as_str()), Some("json-url"));
        assert_eq!(value.get("gff3").and_then(|v| v.as_str()), Some("gff3-url"));
        assert!(value.get("faa").is_none());
        assert!(value.get("logs_txt").is_none());
    }
}

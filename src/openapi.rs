use utoipa::{
    Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

/// Adds the Keycloak bearer token scheme used by the admin API.
struct AdminSecurity;

impl Modify for AdminSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi
            .components
            .get_or_insert_with(Default::default)
            .add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .description(Some(
                            "Keycloak access token of a member of the admin group.",
                        ))
                        .build(),
                ),
            );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Bakta Web Service API",
        description = "Combined OpenAPI document for the stable V1 Bakta API and the extensible V2 workflow API.",
        license(name = "MIT", url = "https://opensource.org/license/mit/")
    ),
    tags(
        (name = "v1", description = "Stable V1 API for the original Bakta genome annotation workflow."),
        (name = "v2", description = "V2 API for multiple workflow kinds including Bakta Proteins and Baktfold."),
        (name = "admin", description = "Admin API: job statistics and Argo workflow operations. Needs a Keycloak access token.")
    ),
    modifiers(&AdminSecurity),
    paths(
        crate::v1::api_paths::delete_job,
        crate::v1::api_paths::job_logs,
        crate::v1::api_paths::init_job,
        crate::v1::api_paths::list_jobs,
        crate::v1::api_paths::query_result,
        crate::v1::api_paths::start_job,
        crate::v1::api_paths::version,
        crate::v2::api_paths::workflows,
        crate::v2::api_paths::init_job,
        crate::v2::api_paths::list_jobs,
        crate::v2::api_paths::query_result,
        crate::v2::api_paths::start_job,
        crate::v2::api_paths::job_logs,
        crate::v2::api_paths::delete_job,
        crate::v2::api_paths::version,
        crate::admin::api_paths::me,
        crate::admin::api_paths::stats_summary,
        crate::admin::api_paths::stats_timeseries,
        crate::admin::api_paths::stats_versions,
        crate::admin::api_paths::stats_runtimes,
        crate::admin::api_paths::stats_inputs,
        crate::admin::api_paths::list_jobs,
        crate::admin::api_paths::get_job,
        crate::admin::api_paths::job_logs,
        crate::admin::operations::retry_job,
        crate::admin::operations::resubmit_job,
        crate::admin::operations::stop_job,
        crate::admin::operations::resume_job,
        crate::admin::operations::delete_job,
        crate::admin::operations::list_workflows,
        crate::admin::operations::bulk_workflows,
    ),
    components(schemas(
        crate::api_structs::Job,
        crate::api_structs::InitRequest,
        crate::api_structs::InitResponse,
        crate::api_structs::RepliconTableType,
        crate::api_structs::ListRequest,
        crate::api_structs::ListResponse,
        crate::api_structs::JobStatusEnum,
        crate::api_structs::JobStatus,
        crate::api_structs::FailedJobStatus,
        crate::api_structs::FailedJobStatusEnum,
        crate::api_structs::ResultFiles,
        crate::api_structs::ResultResponse,
        crate::api_structs::StartRequest,
        crate::api_structs::JobConfig,
        crate::api_structs::DermType,
        crate::api_structs::VersionResponse,
        crate::v2::api_structs::WorkflowKind,
        crate::v2::api_structs::ResultKind,
        crate::v2::api_structs::UploadKind,
        crate::v2::api_structs::DermType,
        crate::v2::api_structs::StageStatus,
        crate::v2::api_structs::JobStatus,
        crate::v2::api_structs::FailedJobStatusKind,
        crate::v2::api_structs::JobReference,
        crate::v2::api_structs::FailedJobStatus,
        crate::v2::api_structs::UploadDescriptor,
        crate::v2::api_structs::UploadLink,
        crate::v2::api_structs::WorkflowDescriptorResponse,
        crate::v2::api_structs::V2InitRequest,
        crate::v2::api_structs::V2InitResponse,
        crate::v2::api_structs::EmptyConfig,
        crate::v2::api_structs::BaktaV2Config,
        crate::v2::api_structs::V2StartRequest,
        crate::v2::api_structs::InputStats,
        crate::v2::api_structs::WorkflowStartConfig,
        crate::v2::api_structs::V2ListRequest,
        crate::v2::api_structs::V2JobStatus,
        crate::v2::api_structs::V2ListResponse,
        crate::v2::api_structs::BaktaResultFiles,
        crate::v2::api_structs::BaktaProteinsResultFiles,
        crate::v2::api_structs::BaktfoldResultFiles,
        crate::v2::api_structs::V2ResultFiles,
        crate::v2::api_structs::V2ResultResponse,
        crate::v2::api_structs::StageLog,
        crate::v2::api_structs::V2LogsResponse,
        crate::v2::api_structs::V2VersionResponse,
        crate::admin::structs::AdminErrorResponse,
        crate::admin::structs::MeResponse,
        crate::admin::structs::WorkflowNodeView,
        crate::admin::structs::WorkflowView,
        crate::admin::structs::JobDetail,
        crate::admin::structs::WorkflowActionResponse,
        crate::admin::structs::DeleteJobResponse,
        crate::admin::structs::WorkflowSummary,
        crate::admin::structs::WorkflowListResponse,
        crate::admin::structs::BulkFilter,
        crate::admin::structs::BulkAction,
        crate::admin::structs::BulkRequest,
        crate::admin::structs::BulkResult,
        crate::admin::structs::BulkItem,
        crate::admin::structs::BulkResponse,
        crate::stats::queries::DurationStats,
        crate::stats::queries::WorkflowKindStats,
        crate::stats::queries::StatsSummary,
        crate::stats::queries::Interval,
        crate::stats::queries::GroupBy,
        crate::stats::queries::TimeseriesBucket,
        crate::stats::queries::Timeseries,
        crate::stats::queries::VersionUsage,
        crate::stats::queries::VersionList,
        crate::stats::queries::JobListItem,
        crate::stats::queries::JobList,
        crate::stats::queries::JobRecord,
        crate::stats::runtimes::KindRuntime,
        crate::stats::runtimes::OverallRuntime,
        crate::stats::runtimes::RuntimeBucket,
        crate::stats::runtimes::InputSizeRuntime,
        crate::stats::runtimes::SecondsPerMbp,
        crate::stats::runtimes::QueueStats,
        crate::stats::runtimes::SlowJob,
        crate::stats::runtimes::RuntimeStats,
        crate::stats::inputs::LengthStats,
        crate::stats::inputs::SequenceCountStats,
        crate::stats::inputs::SizeBucket,
        crate::stats::inputs::SequenceBucket,
        crate::stats::inputs::KindInputs,
        crate::stats::inputs::LargeJob,
        crate::stats::inputs::InputStats,
    ))
)]
pub struct BaktaServiceApi;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v1_and_v2_schemas_do_not_share_names() {
        let doc = serde_json::to_value(BaktaServiceApi::openapi()).unwrap();
        let schemas = &doc["components"]["schemas"];
        assert!(schemas["JobStatus"]["properties"]["jobID"].is_object());
        assert!(schemas["FailedJobStatus"]["properties"]["jobID"].is_object());
        assert!(schemas["DermType"].to_string().contains("MONODERM"));
        assert!(
            schemas["V2JobStatusKind"]
                .to_string()
                .contains("successful")
        );
        assert!(schemas["V2FailedJobStatus"]["properties"]["job_id"].is_object());
        assert!(schemas["V2DermType"].to_string().contains("monoderm"));
        assert_eq!(
            schemas["V2JobStatus"]["properties"]["status"]["$ref"],
            "#/components/schemas/V2JobStatusKind"
        );
    }

    #[test]
    fn test_admin_paths_are_documented_with_bearer_auth() {
        let doc = serde_json::to_value(BaktaServiceApi::openapi()).unwrap();
        assert_eq!(
            doc["components"]["securitySchemes"]["bearer_auth"]["scheme"],
            "bearer"
        );
        let paths = doc["paths"].as_object().unwrap();
        let admin: Vec<_> = paths
            .keys()
            .filter(|path| path.starts_with("/api/admin"))
            .collect();
        assert_eq!(admin.len(), 15, "{admin:?}");
        for (path, item) in paths
            .iter()
            .filter(|(path, _)| path.starts_with("/api/admin"))
        {
            for operation in item.as_object().unwrap().values() {
                assert_eq!(operation["tags"], serde_json::json!(["admin"]), "{path}");
                assert!(operation["security"][0]["bearer_auth"].is_array(), "{path}");
            }
        }
        // The input statistics of the start request and the admin schemas use distinct names.
        let schemas = &doc["components"]["schemas"];
        assert!(schemas["InputStats"]["properties"]["total_length"].is_object());
        assert!(schemas["InputStats"]["properties"]["size_histogram"].is_null());
        assert!(schemas["InputStatistics"]["properties"]["size_histogram"].is_object());
        assert!(schemas["JobRecord"]["properties"]["outcome"].is_object());
    }
}

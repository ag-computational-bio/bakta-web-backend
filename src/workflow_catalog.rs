#![allow(dead_code)]

use crate::v2::api_structs::{ResultKind, UploadKind, WorkflowKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadProfile {
    pub kind: UploadKind,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowDescriptor {
    pub workflow_kind: WorkflowKind,
    pub result_kind: ResultKind,
    pub template_name: &'static str,
    pub uploads: &'static [UploadProfile],
    pub stages: &'static [&'static str],
}

const BAKTA_UPLOADS: [UploadProfile; 6] = [
    UploadProfile {
        kind: UploadKind::GenomeFasta,
        required: true,
    },
    UploadProfile {
        kind: UploadKind::ProdigalTrainingFile,
        required: false,
    },
    UploadProfile {
        kind: UploadKind::RepliconsTable,
        required: false,
    },
    UploadProfile {
        kind: UploadKind::RegionsFile,
        required: false,
    },
    UploadProfile {
        kind: UploadKind::TrustedProteinsFile,
        required: false,
    },
    UploadProfile {
        kind: UploadKind::HmmsFile,
        required: false,
    },
];

const BAKTA_PROTEINS_UPLOADS: [UploadProfile; 1] = [UploadProfile {
    kind: UploadKind::ProteinFasta,
    required: true,
}];

const BAKTFOLD_UPLOADS: [UploadProfile; 1] = [UploadProfile {
    kind: UploadKind::BaktaJson,
    required: true,
}];

const BAKTA_STAGES: [&str; 1] = ["bakta"];
const BAKTA_PROTEINS_STAGES: [&str; 1] = ["bakta_proteins"];
const BAKTA_BAKTFOLD_STAGES: [&str; 2] = ["bakta", "baktfold"];
const BAKTFOLD_STAGES: [&str; 1] = ["baktfold"];

const WORKFLOWS: [WorkflowDescriptor; 4] = [
    WorkflowDescriptor {
        workflow_kind: WorkflowKind::Bakta,
        result_kind: ResultKind::Bakta,
        template_name: "bakta",
        uploads: &BAKTA_UPLOADS,
        stages: &BAKTA_STAGES,
    },
    WorkflowDescriptor {
        workflow_kind: WorkflowKind::BaktaProteins,
        result_kind: ResultKind::BaktaProteins,
        template_name: "bakta-proteins",
        uploads: &BAKTA_PROTEINS_UPLOADS,
        stages: &BAKTA_PROTEINS_STAGES,
    },
    WorkflowDescriptor {
        workflow_kind: WorkflowKind::BaktaBaktfold,
        result_kind: ResultKind::Bakta,
        template_name: "bakta-baktfold",
        uploads: &BAKTA_UPLOADS,
        stages: &BAKTA_BAKTFOLD_STAGES,
    },
    WorkflowDescriptor {
        workflow_kind: WorkflowKind::Baktfold,
        result_kind: ResultKind::Baktfold,
        template_name: "baktfold",
        uploads: &BAKTFOLD_UPLOADS,
        stages: &BAKTFOLD_STAGES,
    },
];

pub fn workflow_descriptors() -> &'static [WorkflowDescriptor] {
    &WORKFLOWS
}

pub fn workflow_descriptor(kind: WorkflowKind) -> WorkflowDescriptor {
    match kind {
        WorkflowKind::Bakta => WORKFLOWS[0],
        WorkflowKind::BaktaProteins => WORKFLOWS[1],
        WorkflowKind::BaktaBaktfold => WORKFLOWS[2],
        WorkflowKind::Baktfold => WORKFLOWS[3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_descriptor_mapping() {
        let bakta = workflow_descriptor(WorkflowKind::Bakta);
        assert_eq!(bakta.result_kind, ResultKind::Bakta);
        assert_eq!(bakta.template_name, "bakta");
        assert_eq!(bakta.stages, ["bakta"]);
        assert_eq!(bakta.uploads.len(), 6);

        let combined = workflow_descriptor(WorkflowKind::BaktaBaktfold);
        assert_eq!(combined.result_kind, ResultKind::Bakta);
        assert_eq!(combined.stages, ["bakta", "baktfold"]);

        let proteins = workflow_descriptor(WorkflowKind::BaktaProteins);
        assert_eq!(proteins.result_kind, ResultKind::BaktaProteins);
        assert_eq!(
            proteins.uploads,
            [UploadProfile {
                kind: UploadKind::ProteinFasta,
                required: true,
            }]
        );

        let baktfold = workflow_descriptor(WorkflowKind::Baktfold);
        assert_eq!(baktfold.result_kind, ResultKind::Baktfold);
        assert_eq!(baktfold.stages, ["baktfold"]);
    }
}

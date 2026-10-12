//! Parsing of the workflow fields that hold job details: the Bakta command
//! line and the workflow template name.

use uuid::Uuid;

use crate::{
    api_structs::{DermType as V1DermType, JobConfig},
    bakta_handler::{ApiVersion, SubmissionDetails},
    stats::JobVersions,
    v2::api_structs::{
        BaktaV2Config, DermType, EmptyConfig, JobReference, V2StartRequest, WorkflowKind,
        WorkflowStartConfig,
    },
};

/// Splits a command line at spaces. Single or double quotes group a value
/// with spaces, like in the shell that runs Bakta.
fn split_command_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    for c in line.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            None => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

fn non_empty(value: String) -> Option<String> {
    Some(value).filter(|value| !value.trim().is_empty())
}

/// The Bakta options of a workflow command line, the reverse of
/// `BaktaV2Config::into_parameters`. Unknown flags are ignored.
pub fn parse_bakta_config(line: &str) -> BaktaV2Config {
    let mut config = BaktaV2Config::default();
    let mut words = split_command_line(line).into_iter();
    while let Some(flag) = words.next() {
        match flag.as_str() {
            "--complete" => config.complete_genome = true,
            "--keep-contig-headers" => config.keep_contig_headers = true,
            "--compliant" => config.compliant = true,
            "--meta" => config.meta = true,
            "--skip-trna" => config.skip_trna = true,
            "--skip-tmrna" => config.skip_tmrna = true,
            "--skip-rrna" => config.skip_rrna = true,
            "--skip-ncrna" => config.skip_ncrna = true,
            "--skip-ncrna-region" => config.skip_ncrna_region = true,
            "--skip-crispr" => config.skip_crispr = true,
            "--skip-cds" => config.skip_cds = true,
            "--skip-pseudo" => config.skip_pseudo = true,
            "--skip-sorf" => config.skip_sorf = true,
            "--skip-gap" => config.skip_gap = true,
            "--skip-ori" => config.skip_ori = true,
            "--skip-filter" => config.skip_filter = true,
            "--skip-plot" => config.skip_plot = true,
            "--min-contig-length"
            | "--prodigal"
            | "--replicons"
            | "--regions"
            | "--proteins"
            | "--hmms"
            | "--locus"
            | "--locus-tag"
            | "--locus-tag-increment"
            | "--genus"
            | "--species"
            | "--strain"
            | "--plasmid"
            | "--translation-table"
            | "--gram" => {
                let Some(value) = words.next() else {
                    break;
                };
                match flag.as_str() {
                    "--min-contig-length" => {
                        config.min_contig_length = value.parse().unwrap_or_default()
                    }
                    "--prodigal" => config.use_prodigal_training_file = true,
                    "--replicons" => config.use_replicons = true,
                    "--regions" => config.use_regions = true,
                    "--proteins" => config.use_trusted_proteins = true,
                    "--hmms" => config.use_hmms = true,
                    "--locus" => config.locus = non_empty(value),
                    "--locus-tag" => config.locus_tag = non_empty(value),
                    "--locus-tag-increment" => {
                        config.locus_tag_increment = value.parse().unwrap_or(1)
                    }
                    "--genus" => config.genus = non_empty(value),
                    "--species" => config.species = non_empty(value),
                    "--strain" => config.strain = non_empty(value),
                    "--plasmid" => config.plasmid = non_empty(value),
                    "--translation-table" => config.translation_table = value.parse().unwrap_or(11),
                    _ => {
                        config.derm_type = Some(match value.as_str() {
                            "+" => DermType::Monoderm,
                            "-" => DermType::Diderm,
                            _ => DermType::Unknown,
                        })
                    }
                }
            }
            _ => {}
        }
    }
    config
}

/// The V1 start request options for the same command line. V1 has fewer
/// options, the others are dropped.
fn v1_config(config: BaktaV2Config) -> JobConfig {
    JobConfig {
        prodigal: config
            .use_prodigal_training_file
            .then(|| "/data/prodigal.tf".to_string()),
        replicons: config.use_replicons,
        table: config.translation_table,
        complete: config.complete_genome,
        headers: config.keep_contig_headers,
        min_length: config.min_contig_length,
        derm: config.derm_type.map(|derm| match derm {
            DermType::Unknown => V1DermType::UNKNOWN,
            DermType::Monoderm => V1DermType::MONODERM,
            DermType::Diderm => V1DermType::DIDERM,
        }),
        genus: config.genus,
        species: config.species,
        strain: config.strain,
        plasmid: config.plasmid,
        locus: config.locus,
        locus_tag: config.locus_tag,
        compliant: config.compliant,
    }
}

/// Organism and `config` of a job, built like at submit from the start
/// request that the command line was made from.
pub fn submission_details(
    kind: WorkflowKind,
    api_version: ApiVersion,
    job_id: Uuid,
    parameter: &str,
) -> SubmissionDetails {
    let workflow = match kind {
        WorkflowKind::Bakta if api_version == ApiVersion::V1 => {
            return SubmissionDetails::from_v1(&v1_config(parse_bakta_config(parameter)));
        }
        WorkflowKind::Bakta => WorkflowStartConfig::Bakta(parse_bakta_config(parameter)),
        WorkflowKind::BaktaBaktfold => {
            WorkflowStartConfig::BaktaBaktfold(parse_bakta_config(parameter))
        }
        WorkflowKind::BaktaProteins => WorkflowStartConfig::BaktaProteins(EmptyConfig {}),
        WorkflowKind::Baktfold => WorkflowStartConfig::Baktfold(EmptyConfig {}),
    };
    SubmissionDetails::from_v2(&V2StartRequest {
        job: JobReference {
            secret: String::new(),
            job_id,
        },
        workflow,
        input_stats: None,
    })
}

/// Workflow kind and tool versions from a template name like
/// `bakta-job-1.12.0` or `bakta-baktfold-job-1.12.2-0.3.0`.
pub fn template_versions(name: &str) -> Option<(WorkflowKind, JobVersions)> {
    let version = |value: &str| Some(value.to_string()).filter(|value| !value.is_empty());
    let (kind, bakta, baktfold) = if let Some(rest) = name.strip_prefix("bakta-baktfold-job-") {
        let (bakta, baktfold) = rest.split_once('-')?;
        (
            WorkflowKind::BaktaBaktfold,
            version(bakta),
            version(baktfold),
        )
    } else if let Some(rest) = name.strip_prefix("bakta-proteins-job-") {
        (WorkflowKind::BaktaProteins, version(rest), None)
    } else if let Some(rest) = name.strip_prefix("bakta-job-") {
        (WorkflowKind::Bakta, version(rest), None)
    } else if let Some(rest) = name.strip_prefix("baktfold-job-") {
        (WorkflowKind::Baktfold, None, version(rest))
    } else {
        return None;
    };
    if bakta.is_none() && baktfold.is_none() {
        return None;
    }
    Some((
        kind,
        JobVersions {
            bakta_version: bakta,
            baktfold_version: baktfold,
            ..Default::default()
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_split_command_line_keeps_quoted_values_together() {
        assert_eq!(
            split_command_line("--strain 'OBrien isolate'  --gram ? --genus \"A b\""),
            [
                "--strain",
                "OBrien isolate",
                "--gram",
                "?",
                "--genus",
                "A b"
            ]
        );
        assert_eq!(split_command_line("--locus ''"), ["--locus", ""]);
        assert!(split_command_line("   ").is_empty());
    }

    #[test]
    fn test_parse_bakta_config_reads_all_options() {
        let config = parse_bakta_config(
            "--min-contig-length 200 --prodigal /data/prodigal.tf --replicons /data/replicons.tsv \
             --regions /data/regions --proteins /data/trusted_proteins.faa --hmms /data/trusted_hmms.hmm \
             --complete --locus 'BSU_00010' --locus-tag 'BSU00010' --locus-tag-increment 5 \
             --keep-contig-headers --genus 'Bacillus' --species 'subtilis' --strain 'OBrien isolate' \
             --plasmid 'pBS32' --compliant --meta --translation-table 25 --gram + --skip-trna --skip-plot",
        );
        let expected = BaktaV2Config {
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
            strain: Some("OBrien isolate".to_string()),
            plasmid: Some("pBS32".to_string()),
            locus: Some("BSU_00010".to_string()),
            locus_tag: Some("BSU00010".to_string()),
            locus_tag_increment: 5,
            compliant: true,
            meta: true,
            skip_trna: true,
            skip_plot: true,
            ..Default::default()
        };
        assert_eq!(config, expected);
        // The command line made from the parsed options is the same again.
        assert_eq!(
            expected.into_parameters().unwrap(),
            "--min-contig-length 200 --prodigal /data/prodigal.tf --replicons /data/replicons.tsv --regions /data/regions --proteins /data/trusted_proteins.faa --hmms /data/trusted_hmms.hmm --complete --locus 'BSU_00010' --locus-tag 'BSU00010' --locus-tag-increment 5 --keep-contig-headers --genus 'Bacillus' --species 'subtilis' --strain 'OBrien isolate' --plasmid 'pBS32' --compliant --meta --translation-table 25 --gram + --skip-trna --skip-plot"
        );
    }

    #[test]
    fn test_parse_bakta_config_of_typical_command_lines() {
        let config = parse_bakta_config(
            "--replicons /data/replicons.tsv --complete --genus 'Pantoea' --gram ?",
        );
        assert!(config.use_replicons && config.complete_genome);
        assert_eq!(config.genus.as_deref(), Some("Pantoea"));
        assert_eq!(config.species, None);
        assert_eq!(config.derm_type, Some(DermType::Unknown));

        let config = parse_bakta_config(
            "--locus-tag 'AC0CIL' --genus 'Pseudomonas' --species 'aeruginosa' --strain 'WARP_W1' --compliant --gram -",
        );
        assert_eq!(config.locus_tag.as_deref(), Some("AC0CIL"));
        assert_eq!(config.strain.as_deref(), Some("WARP_W1"));
        assert!(config.compliant && !config.use_replicons && !config.complete_genome);
        assert_eq!(config.derm_type, Some(DermType::Diderm));
        assert_eq!(config.translation_table, 11);

        // Empty values, unknown flags and a missing last value are ignored.
        let config = parse_bakta_config("--genus '' --unknown --strain");
        assert_eq!(config, BaktaV2Config::default());
        assert_eq!(parse_bakta_config(""), BaktaV2Config::default());
    }

    #[test]
    fn test_submission_details_match_the_submit_shape() {
        let id = Uuid::new_v4();
        let line = "--prodigal /data/prodigal.tf --replicons /data/replicons.tsv --genus 'Escherichia' --species 'coli' --strain 'K-12 MG1655' --gram ?";

        let v1 = submission_details(WorkflowKind::Bakta, ApiVersion::V1, id, line);
        assert_eq!(v1.genus.as_deref(), Some("Escherichia"));
        assert_eq!(v1.species.as_deref(), Some("coli"));
        assert_eq!(v1.strain.as_deref(), Some("K-12 MG1655"));
        let config = v1.config.unwrap();
        assert_eq!(config["hasProdigalTrainingFile"], json!(true));
        assert_eq!(config["hasReplicons"], json!(true));
        assert_eq!(config["dermType"], json!("UNKNOWN"));
        assert!(config.get("prodigalTrainingFile").is_none());

        let v2 = submission_details(WorkflowKind::BaktaBaktfold, ApiVersion::V2, id, line);
        assert_eq!(v2.strain.as_deref(), Some("K-12 MG1655"));
        let config = v2.config.unwrap();
        assert_eq!(config["use_prodigal_training_file"], json!(true));
        assert_eq!(config["genus"], json!("Escherichia"));

        let proteins = submission_details(WorkflowKind::BaktaProteins, ApiVersion::V2, id, "");
        assert_eq!(proteins.genus, None);
        assert_eq!(proteins.config, Some(json!({})));
        let baktfold = submission_details(WorkflowKind::Baktfold, ApiVersion::V2, id, "");
        assert_eq!(baktfold.config, Some(json!({})));
    }

    #[test]
    fn test_template_versions() {
        let versions = |bakta: Option<&str>, baktfold: Option<&str>| JobVersions {
            bakta_version: bakta.map(str::to_string),
            baktfold_version: baktfold.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(
            template_versions("bakta-job-1.11.3"),
            Some((WorkflowKind::Bakta, versions(Some("1.11.3"), None)))
        );
        assert_eq!(
            template_versions("bakta-proteins-job-1.12.2"),
            Some((WorkflowKind::BaktaProteins, versions(Some("1.12.2"), None)))
        );
        assert_eq!(
            template_versions("bakta-baktfold-job-1.12.2-0.3.0"),
            Some((
                WorkflowKind::BaktaBaktfold,
                versions(Some("1.12.2"), Some("0.3.0"))
            ))
        );
        assert_eq!(
            template_versions("baktfold-job-0.1.1"),
            Some((WorkflowKind::Baktfold, versions(None, Some("0.1.1"))))
        );
        assert_eq!(template_versions("bakta-job-"), None);
        assert_eq!(template_versions("bakta-baktfold-job-1.12.2"), None);
        assert_eq!(template_versions("other-template"), None);
    }
}

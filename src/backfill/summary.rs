//! Bakta's small `.txt` summary next to the result JSON.

/// Name of the summary in `jobs/<id>/results/`. The templates run Bakta with
/// `--prefix result`.
pub const SUMMARY_FILE: &str = "result.txt";

/// Input size and database version from a Bakta summary. Missing or
/// unreadable values stay `None`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Summary {
    pub total_length: Option<i64>,
    pub sequence_count: Option<i32>,
    pub db_version: Option<String>,
}

/// Reads the `Length`, `Count` and `Database` lines, for example
/// `Database: v6.0, full` gives the database version `6.0`.
pub fn parse_summary(text: &str) -> Summary {
    let mut summary = Summary::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Length" if summary.total_length.is_none() => {
                summary.total_length = value.parse().ok();
            }
            "Count" if summary.sequence_count.is_none() => {
                summary.sequence_count = value.parse().ok();
            }
            "Database" if summary.db_version.is_none() => {
                let version = value.split(',').next().unwrap_or_default().trim();
                let version = version.strip_prefix('v').unwrap_or(version);
                summary.db_version = Some(version.to_string()).filter(|v| !v.is_empty());
            }
            _ => {}
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Summary of a Bakta 1.12 run, as written by `bakta/main.py`.
    const EXAMPLE: &str = "Sequence(s):
Length: 4641652
Count: 1
GC: 50.8
N50: 4641652
N90: 4641652
N ratio: 0.0
coding density: 87.9

Annotation:
tRNAs: 86
tmRNAs: 1
rRNAs: 22
ncRNAs: 72
ncRNA regions: 66
CRISPR arrays: 2
CDSs: 4317
pseudogenes: 26
hypotheticals: 37
sORFs: 2
gaps: 0
oriCs: 1
oriVs: 0
oriTs: 0

Bakta:
Software: v1.12.0
Database: v6.0, full
DOI: 10.1099/mgen.0.000685
URL: github.com/oschwengers/bakta
";

    #[test]
    fn test_parse_summary_of_a_bakta_run() {
        assert_eq!(
            parse_summary(EXAMPLE),
            Summary {
                total_length: Some(4_641_652),
                sequence_count: Some(1),
                db_version: Some("6.0".to_string()),
            }
        );
    }

    #[test]
    fn test_parse_summary_leaves_missing_values_empty() {
        assert_eq!(parse_summary(""), Summary::default());
        assert_eq!(
            parse_summary("Length: many\nCount: 12\nDatabase: \n"),
            Summary {
                sequence_count: Some(12),
                ..Default::default()
            }
        );
        assert_eq!(parse_summary("{\"not\": \"a summary\"}").total_length, None);
    }
}

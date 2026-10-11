//! Tests for the statistics queries against a real Postgres.

use super::{
    StatsDb,
    queries::*,
    test_db::{test_db, unique_environment},
};
use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

struct Row {
    kind: &'static str,
    status: &'static str,
    outcome: Option<&'static str>,
    created: DateTime<Utc>,
    runtime: Option<f64>,
    queue: Option<f64>,
    length: Option<i64>,
    genus: Option<&'static str>,
    bakta_version: Option<&'static str>,
}

fn row(kind: &'static str, status: &'static str, created: DateTime<Utc>) -> Row {
    let outcome = match status {
        "succeeded" | "failed" | "error" => Some(status),
        _ => None,
    };
    Row {
        kind,
        status,
        outcome,
        created,
        runtime: None,
        queue: None,
        length: None,
        genus: None,
        bakta_version: None,
    }
}

async fn insert(db: &StatsDb, environment: &str, row: Row) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs (id, environment, workflow_kind, api_version, status, outcome, name,
             genus, species, total_length, bakta_version, created_at, finished_at,
             runtime_seconds, queue_seconds)
         VALUES ($1, $2, $3, 'v2', $4, $5, 'job name', $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(id)
    .bind(environment)
    .bind(row.kind)
    .bind(row.status)
    .bind(row.outcome)
    .bind(row.genus)
    .bind(row.genus.map(|_| "coli"))
    .bind(row.length)
    .bind(row.bakta_version)
    .bind(row.created)
    .bind(row.outcome.map(|_| row.created))
    .bind(row.runtime)
    .bind(row.queue)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

fn day(month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, month, day, 12, 0, 0).unwrap()
}

fn filter(environment: &str) -> StatsFilter {
    StatsFilter {
        environment: Some(environment.to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn summary_counts_by_outcome_and_computes_percentiles() {
    let Some(db) = test_db("summary_counts_by_outcome").await else {
        return;
    };
    let env = unique_environment();
    for runtime in [100.0, 200.0, 300.0, 400.0] {
        insert(
            &db,
            &env,
            Row {
                runtime: Some(runtime),
                queue: Some(10.0),
                length: Some(1000),
                ..row("bakta", "succeeded", day(9, 1))
            },
        )
        .await;
    }
    insert(&db, &env, row("bakta", "failed", day(9, 2))).await;
    // Deleted after it failed: still counted as failed by outcome.
    insert(
        &db,
        &env,
        Row {
            outcome: Some("error"),
            ..row("baktfold", "deleted", day(9, 3))
        },
    )
    .await;
    insert(&db, &env, row("baktfold", "running", day(9, 4))).await;
    insert(&db, &env, row("bakta", "created", day(9, 5))).await;
    insert(
        &db,
        &unique_environment(),
        row("bakta", "succeeded", day(9, 5)),
    )
    .await;

    let summary = db.summary(&filter(&env)).await.unwrap();
    assert_eq!(summary.jobs_total, 8);
    assert_eq!(summary.jobs_finished, 6);
    assert_eq!(summary.jobs_succeeded, 4);
    assert_eq!(summary.jobs_failed, 1);
    assert_eq!(summary.jobs_error, 1);
    assert_eq!(summary.jobs_active, 1);
    assert_eq!(summary.success_rate, Some(4.0 / 6.0));
    assert_eq!(summary.runtime_seconds.avg, Some(250.0));
    assert_eq!(summary.runtime_seconds.p50, Some(250.0));
    assert_eq!(summary.queue_seconds.p90, Some(10.0));
    assert_eq!(summary.total_input_bp, 4000);
    assert_eq!(summary.by_workflow_kind.len(), 2);
    assert_eq!(summary.by_workflow_kind[0].workflow_kind, "bakta");
    assert_eq!(summary.by_workflow_kind[0].jobs_total, 6);
    assert_eq!(summary.by_workflow_kind[0].jobs_succeeded, 4);
    assert_eq!(summary.by_workflow_kind[1].runtime_seconds.avg, None);

    let ranged = db
        .summary(&StatsFilter {
            from: Some(day(9, 2)),
            to: Some(day(9, 4)),
            ..filter(&env)
        })
        .await
        .unwrap();
    assert_eq!(ranged.jobs_total, 2);

    let empty = db.summary(&filter(&unique_environment())).await.unwrap();
    assert_eq!(empty.jobs_total, 0);
    assert_eq!(empty.success_rate, None);
    assert_eq!(empty.runtime_seconds, DurationStats::default());
}

#[tokio::test]
async fn timeseries_groups_by_interval_and_column() {
    let Some(db) = test_db("timeseries_groups").await else {
        return;
    };
    let env = unique_environment();
    insert(&db, &env, row("bakta", "succeeded", day(8, 30))).await;
    insert(&db, &env, row("bakta", "failed", day(9, 1))).await;
    insert(&db, &env, row("baktfold", "succeeded", day(9, 20))).await;

    let monthly = db
        .timeseries(&filter(&env), Interval::Month, GroupBy::None)
        .await
        .unwrap();
    assert_eq!(monthly.buckets.len(), 2);
    assert_eq!(
        monthly.buckets[0].period_start,
        Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap()
    );
    assert_eq!(monthly.buckets[0].group, None);
    assert_eq!(monthly.buckets[1].jobs_total, 2);
    assert_eq!(monthly.buckets[1].jobs_failed, 1);

    let grouped = db
        .timeseries(&filter(&env), Interval::Year, GroupBy::WorkflowKind)
        .await
        .unwrap();
    let groups: Vec<_> = grouped
        .buckets
        .iter()
        .map(|bucket| (bucket.group.clone().unwrap(), bucket.jobs_total))
        .collect();
    assert_eq!(
        groups,
        [("bakta".to_string(), 2), ("baktfold".to_string(), 1)]
    );

    let daily = db
        .timeseries(&filter(&env), Interval::Day, GroupBy::Status)
        .await
        .unwrap();
    assert_eq!(daily.buckets.len(), 3);
}

#[tokio::test]
async fn versions_lists_version_combinations() {
    let Some(db) = test_db("versions_lists_version_combinations").await else {
        return;
    };
    let env = unique_environment();
    for (version, created) in [
        ("1.11.0", day(7, 1)),
        ("1.12.2", day(9, 1)),
        ("1.12.2", day(9, 3)),
    ] {
        insert(
            &db,
            &env,
            Row {
                bakta_version: Some(version),
                ..row("bakta", "succeeded", created)
            },
        )
        .await;
    }
    insert(&db, &env, row("bakta", "created", day(9, 4))).await;
    let versions = db.versions(&filter(&env)).await.unwrap();
    assert_eq!(versions.items.len(), 2);
    assert_eq!(versions.items[0].bakta_version.as_deref(), Some("1.12.2"));
    assert_eq!(versions.items[0].jobs_total, 2);
    assert_eq!(versions.items[0].first_seen, day(9, 1));
    assert_eq!(versions.items[0].last_seen, day(9, 3));
}

#[tokio::test]
async fn list_jobs_filters_searches_sorts_and_pages() {
    let Some(db) = test_db("list_jobs_filters").await else {
        return;
    };
    let env = unique_environment();
    let coli = insert(
        &db,
        &env,
        Row {
            genus: Some("Escherichia"),
            runtime: Some(50.0),
            length: Some(10),
            ..row("bakta", "succeeded", day(9, 1))
        },
    )
    .await;
    let slow = insert(
        &db,
        &env,
        Row {
            runtime: Some(500.0),
            length: Some(30),
            ..row("bakta", "failed", day(9, 2))
        },
    )
    .await;
    let fold = insert(&db, &env, row("baktfold", "running", day(9, 3))).await;

    let query = |q: JobListQuery| JobListQuery {
        filter: filter(&env),
        limit: if q.limit == 0 { 50 } else { q.limit },
        ..q
    };
    let all = db.list_jobs(&query(JobListQuery::default())).await.unwrap();
    assert_eq!(all.total, 3);
    let ids: Vec<_> = all.items.iter().map(|item| item.id).collect();
    assert_eq!(ids, [fold, slow, coli]);
    assert_eq!(all.items[2].organism.as_deref(), Some("Escherichia coli"));
    assert_eq!(all.items[0].organism, None);

    let by_runtime = db
        .list_jobs(&query(JobListQuery {
            sort: "-runtime_seconds".parse().unwrap(),
            ..Default::default()
        }))
        .await
        .unwrap();
    let ids: Vec<_> = by_runtime.items.iter().map(|item| item.id).collect();
    assert_eq!(ids, [slow, coli, fold]);

    let oldest_first = db
        .list_jobs(&query(JobListQuery {
            sort: "-created_at".parse().unwrap(),
            limit: 1,
            offset: 1,
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(oldest_first.total, 3);
    assert_eq!(oldest_first.items.len(), 1);
    assert_eq!(oldest_first.items[0].id, slow);

    let filtered = db
        .list_jobs(&query(JobListQuery {
            statuses: vec!["succeeded".to_string(), "failed".to_string()],
            workflow_kinds: vec!["bakta".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap();
    assert_eq!(filtered.total, 2);

    for q in ["escherichia COLI", &coli.to_string()[..8], "%"] {
        let found = db
            .list_jobs(&query(JobListQuery {
                q: Some(q.to_string()),
                ..Default::default()
            }))
            .await
            .unwrap();
        let expected = if q == "%" { 0 } else { 1 };
        assert_eq!(found.total, expected, "{q}");
    }

    assert_eq!("created_at".parse::<JobSort>().unwrap(), JobSort::default());
    assert!("name".parse::<JobSort>().is_err());
}

#[tokio::test]
async fn get_job_returns_the_full_record() {
    let Some(db) = test_db("get_job_returns_the_full_record").await else {
        return;
    };
    let env = unique_environment();
    let id = insert(&db, &env, row("bakta", "succeeded", day(9, 1))).await;
    let record = db.get_job(id).await.unwrap().unwrap();
    assert_eq!(record.id, id);
    assert_eq!(record.environment, env);
    assert_eq!(record.outcome.as_deref(), Some("succeeded"));
    assert!(db.get_job(Uuid::new_v4()).await.unwrap().is_none());
}

#[tokio::test]
async fn runtimes_use_succeeded_jobs_and_fixed_buckets() {
    let Some(db) = test_db("runtimes_use_succeeded_jobs_and_fixed_buckets").await else {
        return;
    };
    let env = unique_environment();
    for (runtime, length, created) in [
        (100.0, 500_000, day(9, 1)),
        (200.0, 1_000_000, day(9, 2)),
        (300.0, 2_000_000, day(9, 3)),
        (5000.0, 5_000_000, day(9, 4)),
    ] {
        insert(
            &db,
            &env,
            Row {
                runtime: Some(runtime),
                length: Some(length),
                genus: Some("Escherichia"),
                queue: Some(10.0),
                ..row("bakta", "succeeded", created)
            },
        )
        .await;
    }
    insert(
        &db,
        &env,
        Row {
            runtime: Some(50.0),
            ..row("bakta", "failed", day(9, 5))
        },
    )
    .await;
    insert(
        &db,
        &env,
        Row {
            runtime: Some(700.0),
            ..row("baktfold", "succeeded", day(9, 6))
        },
    )
    .await;
    insert(
        &db,
        &env,
        Row {
            queue: Some(600.0),
            ..row("baktfold", "running", day(9, 7))
        },
    )
    .await;

    let stats = db.runtimes(&filter(&env), &[]).await.unwrap();
    assert_eq!(stats.jobs, 5);
    let bakta = &stats.by_workflow_kind[0];
    assert_eq!((bakta.workflow_kind.as_str(), bakta.jobs), ("bakta", 4));
    assert_eq!(
        (bakta.min, bakta.max, bakta.p50),
        (Some(100.0), Some(5000.0), Some(250.0))
    );
    assert!(bakta.stddev.is_some());
    let histogram: Vec<_> = stats.histogram.iter().map(|bucket| bucket.jobs).collect();
    assert_eq!(histogram, [1, 1, 1, 1, 0, 0, 1, 0, 0]);
    assert_eq!(stats.histogram[8].max_seconds, None);
    assert_eq!(stats.histogram[1].max_seconds, Some(300));
    let sizes: Vec<_> = stats
        .by_input_size
        .iter()
        .map(|bucket| bucket.jobs)
        .collect();
    assert_eq!(sizes, [1, 1, 1, 1, 0, 0, 0]);
    assert_eq!(stats.by_input_size[0].runtime_p50, Some(100.0));
    assert_eq!(stats.by_input_size[4].runtime_avg, None);
    assert_eq!(stats.by_input_size[6].max_bp, None);
    // 100 s / 0.5 Mbp, 200 / 1, 300 / 2, 5000 / 5 -> 200, 200, 150, 1000.
    assert_eq!(stats.seconds_per_mbp.p50, Some(200.0));
    assert_eq!(stats.seconds_per_mbp.avg, Some(387.5));
    assert_eq!(stats.queue_seconds.max, Some(600.0));
    assert_eq!(stats.slowest.len(), 5);
    assert_eq!(stats.slowest[0].runtime_seconds, 5000.0);
    assert_eq!(
        stats.slowest[0].organism.as_deref(),
        Some("Escherichia coli")
    );

    let fold_only = db
        .runtimes(&filter(&env), &["baktfold".to_string()])
        .await
        .unwrap();
    assert_eq!(fold_only.jobs, 1);
    assert_eq!(fold_only.queue_seconds.max, Some(600.0));

    let empty = db
        .runtimes(&filter(&unique_environment()), &[])
        .await
        .unwrap();
    assert_eq!(empty.jobs, 0);
    assert!(empty.by_workflow_kind.is_empty());
    assert_eq!(empty.histogram.len(), 9);
    assert_eq!(empty.by_input_size.len(), 7);
    assert_eq!(empty.seconds_per_mbp.p50, None);
    assert_eq!(empty.queue_seconds.avg, None);

    let series = db
        .timeseries(&filter(&env), Interval::Year, GroupBy::None)
        .await
        .unwrap();
    assert_eq!(series.buckets[0].runtime_p50, Some(300.0));
    assert!(series.buckets[0].runtime_p90.unwrap() > 300.0);
}

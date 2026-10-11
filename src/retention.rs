//! S3 data retention: job data is deleted a fixed number of days after the job.

use chrono::{DateTime, Duration, Utc};

pub const DEFAULT_DATA_RETENTION_DAYS: i64 = 30;

/// Reference time of the job data: the finish time, or the creation time of a job that never finished.
pub fn reference_time(
    finished_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
) -> DateTime<Utc> {
    finished_at.unwrap_or(created_at)
}

pub fn data_expires_at(reference: DateTime<Utc>, retention_days: i64) -> DateTime<Utc> {
    reference + Duration::days(retention_days)
}

/// True while `now` is before the expiry time. At the expiry time the data is gone.
pub fn is_data_available(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now < expires_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn data_is_available_until_the_expiry_time() {
        let reference = Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap();
        let expires = data_expires_at(reference, 30);
        assert_eq!(
            expires,
            Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap()
        );
        assert!(is_data_available(expires, expires - Duration::seconds(1)));
        assert!(!is_data_available(expires, expires));
        assert!(!is_data_available(expires, expires + Duration::seconds(1)));
    }

    #[test]
    fn reference_time_is_the_finish_time_or_the_creation_time() {
        let created = Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).unwrap();
        let finished = created + Duration::hours(1);
        assert_eq!(reference_time(Some(finished), created), finished);
        assert_eq!(reference_time(None, created), created);
    }
}

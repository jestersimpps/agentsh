use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_AGO_SECS: f64 = 1e11;
const MAX_WAIT_SECS: f64 = 1e7;

pub fn iso(time: SystemTime) -> String {
    let datetime = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => DateTime::from_timestamp(after.as_secs() as i64, after.subsec_nanos()),
        Err(before) => DateTime::from_timestamp(-(before.duration().as_secs() as i64), 0),
    };
    format_utc(datetime.unwrap_or(DateTime::<Utc>::MIN_UTC))
}

pub fn iso_now() -> String {
    format_utc(Utc::now())
}

pub fn iso_ago(seconds: f64) -> String {
    let secs = if seconds.is_finite() { seconds.clamp(0.0, MAX_AGO_SECS) } else { MAX_AGO_SECS };
    let then = TimeDelta::try_seconds(secs as i64).and_then(|delta| Utc::now().checked_sub_signed(delta));
    format_utc(then.unwrap_or(DateTime::<Utc>::MIN_UTC))
}

pub fn wait_duration(seconds: f64) -> Duration {
    let secs = if seconds.is_finite() { seconds.clamp(0.01, MAX_WAIT_SECS) } else { MAX_WAIT_SECS };
    Duration::from_secs_f64(secs)
}

fn format_utc(datetime: DateTime<Utc>) -> String {
    datetime.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extreme_inputs_do_not_panic() {
        assert!(!iso_ago(9.2e18).is_empty());
        assert!(!iso_ago(f64::NAN).is_empty());
        assert_eq!(wait_duration(1e30), Duration::from_secs_f64(MAX_WAIT_SECS));
        if let Some(ancient) = UNIX_EPOCH.checked_sub(Duration::from_secs(1u64 << 62)) {
            assert!(!iso(ancient).is_empty());
        }
    }
}

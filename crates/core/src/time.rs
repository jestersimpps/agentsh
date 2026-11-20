use chrono::{DateTime, SecondsFormat, Utc};
use std::time::{Duration, SystemTime};

pub fn iso(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn iso_now() -> String {
    iso(SystemTime::now())
}

pub fn iso_ago(seconds: f64) -> String {
    let ago = Duration::from_secs_f64(seconds.max(0.0));
    iso(SystemTime::now().checked_sub(ago).unwrap_or(SystemTime::UNIX_EPOCH))
}

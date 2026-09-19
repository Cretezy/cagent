/// Formats a Unix-millisecond timestamp relative to a supplied current time.
#[must_use]
pub fn relative_time(timestamp: &str, now_millis: u128) -> String {
    let timestamp = timestamp.parse::<u128>().unwrap_or(now_millis);
    let seconds = now_millis.saturating_sub(timestamp) / 1_000;
    match seconds {
        0..=9 => "now".into(),
        10..=59 => format!("{seconds}s ago"),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        86_400..=604_799 => format!("{}d ago", seconds / 86_400),
        _ => format!("{}w ago", seconds / 604_800),
    }
}

/// Formats a completed turn duration for the durable transcript.
#[must_use]
pub fn format_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}h {:02}m {:02}s",
            seconds / 3_600,
            (seconds % 3_600) / 60,
            seconds % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{format_elapsed, relative_time};

    #[test]
    fn formats_compact_relative_times() {
        let now = 10_000_000;
        assert_eq!(relative_time("9990000", now), "10s ago");
        assert_eq!(relative_time("9820000", now), "3m ago");
        assert_eq!(relative_time("9840000", now), "2m ago");
        assert_eq!(relative_time("-", now), "now");
    }

    #[test]
    fn formats_elapsed_durations_compactly() {
        assert_eq!(format_elapsed(9), "9s");
        assert_eq!(format_elapsed(60), "1m 00s");
        assert_eq!(format_elapsed(61), "1m 01s");
        assert_eq!(format_elapsed(3_600), "1h 00m 00s");
        assert_eq!(format_elapsed(3_661), "1h 01m 01s");
    }
}

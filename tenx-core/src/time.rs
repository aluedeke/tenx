use std::time::{Duration, SystemTime};

/// Coarse human age — "12m", "3h", "2d", "1w", "3mo". One unit, no decimals:
/// this is a glance value in a list column, not a timestamp.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 86400 * 7 {
        format!("{}d", secs / 86400)
    } else if secs < 86400 * 30 {
        format!("{}w", secs / (86400 * 7))
    } else {
        format!("{}mo", secs / (86400 * 30))
    }
}

/// `format_duration` of how long ago `t` was (clock skew reads as "0m").
pub fn format_age(t: SystemTime) -> String {
    format_duration(t.elapsed().unwrap_or_default())
}

/// Parse a plain "<N><unit>" duration — "30m", "4h", "2d". No combined units;
/// this is a CLI flag, not a date library.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    if s.is_empty() {
        return Err("empty duration".into());
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid duration '{s}' (want e.g. '4h')"))?;
    let secs = match unit {
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return Err(format!("duration '{s}' must end in m/h/d")),
    };
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_format_by_bucket() {
        assert_eq!(format_duration(Duration::from_secs(59)), "0m");
        assert_eq!(format_duration(Duration::from_secs(45 * 60)), "45m");
        assert_eq!(format_duration(Duration::from_secs(5 * 3600)), "5h");
        assert_eq!(format_duration(Duration::from_secs(3 * 86400)), "3d");
        assert_eq!(format_duration(Duration::from_secs(10 * 86400)), "1w");
        assert_eq!(format_duration(Duration::from_secs(95 * 86400)), "3mo");
    }

    #[test]
    fn parse_accepts_m_h_d() {
        assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("4h"), Ok(Duration::from_secs(4 * 3600)));
        assert_eq!(parse_duration("2d"), Ok(Duration::from_secs(2 * 86400)));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("4x").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("1h30m").is_err());
    }
}

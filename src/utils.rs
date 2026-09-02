use worker::Result;

pub fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap();
    
    format!("{}-{}", now.as_secs(), now.subsec_millis())
}

pub fn current_timestamp() -> String {
    // TODO: Use proper datetime library
    chrono::Utc::now().to_rfc3339()
}

pub fn validate_url(url: &str) -> Result<()> {
    // TODO: Implement URL validation
    let _ = url;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_id_has_seconds_and_millis_shape() {
        let id = generate_id();

        // Expected format: "<unix-seconds>-<milliseconds>"
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 2, "id should contain exactly one '-' separator: {id}");

        let seconds: u64 = parts[0].parse().expect("seconds part should be numeric");
        let millis: u64 = parts[1].parse().expect("millis part should be numeric");

        // Seconds should be near the current epoch and millis within 0..=999.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!((now_secs as i64 - seconds as i64).abs() <= 5);
        assert!(millis <= 999);
    }

    #[test]
    fn generate_id_is_never_empty() {
        for _ in 0..100 {
            assert!(!generate_id().is_empty());
        }
    }

    #[test]
    fn current_timestamp_is_parseable_rfc3339() {
        let ts = current_timestamp();
        let parsed = chrono::DateTime::parse_from_rfc3339(&ts)
            .expect("timestamp should be RFC 3339");
        assert_eq!(parsed.to_rfc3339(), ts);
    }

    #[test]
    fn current_timestamp_reflects_utc() {
        let ts = current_timestamp();
        // to_rfc3339 emits `+00:00` for a UTC chrono::DateTime.
        assert!(ts.ends_with("+00:00") || ts.ends_with('Z'), "unexpected ts format: {ts}");
    }

    #[test]
    fn validate_url_accepts_everything_for_now() {
        // `validate_url` is currently a documented stub that always succeeds.
        assert!(validate_url("https://example.com/feed.xml").is_ok());
        assert!(validate_url("http://localhost:8080/rss").is_ok());
        assert!(validate_url("not a url at all").is_ok());
        assert!(validate_url("").is_ok());
        assert!(validate_url("javascript:alert(1)").is_ok());
    }
}

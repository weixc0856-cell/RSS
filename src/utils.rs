pub fn current_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_timestamp_is_parseable_rfc3339() {
        let ts = current_timestamp();
        let parsed =
            chrono::DateTime::parse_from_rfc3339(&ts).expect("timestamp should be RFC 3339");
        assert_eq!(parsed.to_rfc3339(), ts);
    }

    #[test]
    fn current_timestamp_reflects_utc() {
        let ts = current_timestamp();
        // to_rfc3339 emits `+00:00` for a UTC chrono::DateTime.
        assert!(ts.ends_with("+00:00") || ts.ends_with('Z'), "unexpected ts format: {ts}");
    }
}


pub fn current_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// SQLite-flavoured UTC timestamp `YYYY-MM-DD HH:MM:SS` — lexicographically
/// comparable with `datetime('now')` as used across the D1 queries.
pub fn sqlite_now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `sqlite_now()` plus `minutes` (used for `next_fetch_at` / backoff).
pub fn sqlite_now_plus_minutes(minutes: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(minutes))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Canonical feed identity: lower-cased scheme/host, default port dropped,
/// fragment removed and trailing "/" stripped from the path (kept for "/").
/// Query strings are preserved — Google News RSS search URLs rely on them.
pub fn canonical_url(raw: &str) -> String {
    let trimmed = raw.trim();
    match url::Url::parse(trimmed) {
        Ok(mut u) => {
            let _ = u.set_fragment(None);
            let scheme = u.scheme().to_lowercase();
            let host = u
                .host_str()
                .map(|h| h.to_lowercase())
                .unwrap_or_default();
            let default_port_removed = matches!(
                (scheme.as_str(), u.port()),
                ("http", Some(80)) | ("https", Some(443))
            );
            if default_port_removed {
                let _ = u.set_port(None);
            }
            let path = u.path();
            let path = if path.len() > 1 && path.ends_with('/') {
                path.trim_end_matches('/')
            } else {
                path
            };
            let query = u.query().map(|q| format!("?{q}")).unwrap_or_default();
            format!("{scheme}://{host}{path}{query}")
        }
        Err(_) => trimmed.trim_end_matches('/').to_string(),
    }
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

    #[test]
    fn sqlite_now_matches_expected_shape() {
        let s = sqlite_now();
        assert_eq!(s.len(), 19, "expected YYYY-MM-DD HH:MM:SS, got {s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], " ");
    }

    #[test]
    fn sqlite_now_plus_minutes_adds_offset() {
        // Minute precision: only compare against a fresh now().
        let later = sqlite_now_plus_minutes(30);
        assert!(later > sqlite_now(), "offset string should sort after now");
    }

    #[test]
    fn canonical_url_strips_trailing_slash_and_case() {
        let a = canonical_url("https://Feeds.BBCi.co.uk/news/rss.xml");
        let b = canonical_url("https://feeds.bbci.co.uk/news/rss.xml/");
        assert_eq!(a, "https://feeds.bbci.co.uk/news/rss.xml");
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_url_preserves_query_but_drops_fragment_and_port() {
        let q = canonical_url(
            "https://news.google.com/rss/search?q=Anthropic+Claude&hl=en-US&gl=US&ceid=US:en#top",
        );
        assert_eq!(
            q,
            "https://news.google.com/rss/search?q=Anthropic+Claude&hl=en-US&gl=US&ceid=US:en"
        );
        let port = canonical_url("https://example.com:443/feed");
        assert_eq!(port, "https://example.com/feed");
    }
}


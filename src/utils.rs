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

/// Normalize an RSS/Atom published timestamp to a canonical, chronologically
/// sortable UTC ISO string `YYYY-MM-DDTHH:MM:SSZ`. The 20-char fixed shape is
/// the sort contract for `ORDER BY published_at DESC` / `MAX(published_at)`:
/// lexicographic == chronological only while every row obeys this exact form.
///
/// Accepts RFC1123/RFC2822 RSS `pubDate` (`Wed, 02 Sep 2026 12:00:00 GMT`) and
/// RFC3339/Atom ISO dates (`Z` / `+hh:mm` / `+hhmm`). Returns `None` when
/// unparseable — the caller must keep the original value (no data loss) and
/// log, so such a row simply has no sortable-time guarantee.
pub fn normalize_published_at(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // 1) Already ISO / RFC3339 (Atom feeds) — idempotent.
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(to_utc_z(dt));
    }
    // 2) RSS RFC1123/RFC2822 pubDate. chrono's RFC2822 parser does not accept
    //    the textual zones (`GMT`/`UT`/`UTC`) dependably, so map a trailing one
    //    to a numeric `+0000` first (case-insensitive); `±hhmm` is native.
    let mut candidate = raw.to_string();
    let upper = raw.to_ascii_uppercase();
    for zone in ["GMT", "UT", "UTC"] {
        if upper.ends_with(zone) {
            let cut = candidate.len() - zone.len();
            candidate.truncate(cut);
            candidate.push_str("+0000");
            break;
        }
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(&candidate) {
        return Some(to_utc_z(dt));
    }
    None
}

/// Canonical UTC form: `YYYY-MM-DDTHH:MM:SSZ` — whole seconds, `Z` suffix, so a
/// string comparison sorts by real time. (JS `toISOString()` minus millis is
/// byte-identical, keeping the Rust writer and the backfill script in lockstep.)
fn to_utc_z(dt: chrono::DateTime<chrono::FixedOffset>) -> String {
    dt.with_timezone(&chrono::Utc)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
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

    /// RSS RFC1123/RFC2822 `pubDate` textual-zone variants all collapse to the
    /// canonical `YYYY-MM-DDTHH:MM:SSZ` UTC form (lexicographic == chronological).
    #[test]
    fn normalize_published_at_rss_rfc822_variants() {
        // Textual zones: named `GMT` + numeric offsets, both real corpus shapes.
        assert_eq!(
            normalize_published_at("Wed, 02 Sep 2026 12:00:00 GMT").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("Wed, 02 Sep 2026 12:00:00 UT").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("Wed, 02 Sep 2026 12:00:00 UTC").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("Wed, 02 Sep 2026 12:00:00 +0000").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("wed, 02 sep 2026 12:00:00 gmt").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        // Non-UTC offsets are shifted to UTC, not kept verbatim.
        assert_eq!(
            normalize_published_at("Wed, 02 Sep 2026 12:00:00 -0500").as_deref(),
            Some("2026-09-02T17:00:00Z")
        );
        assert_eq!(
            normalize_published_at("Wed, 02 Sep 2026 12:00:00 +0530").as_deref(),
            Some("2026-09-02T06:30:00Z")
        );
        // Real corpus samples (the ones the stale-window bug surfaced).
        assert_eq!(
            normalize_published_at("Wed, 31 May 2023 07:00:00 GMT").as_deref(),
            Some("2023-05-31T07:00:00Z")
        );
        assert_eq!(
            normalize_published_at("Thu, 03 Sep 2026 08:14:29 GMT").as_deref(),
            Some("2026-09-03T08:14:29Z")
        );
    }

    /// RFC3339/Atom ISO inputs are already sortable — normalization is idempotent
    /// for `Z` and converts explicit offsets to the canonical UTC `Z` form.
    #[test]
    fn normalize_published_at_iso_idempotent() {
        assert_eq!(
            normalize_published_at("2026-09-02T12:00:00Z").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("2026-08-15T09:30:00+00:00").as_deref(),
            Some("2026-08-15T09:30:00Z")
        );
        assert_eq!(
            normalize_published_at("2026-09-02T12:00:00-05:00").as_deref(),
            Some("2026-09-02T17:00:00Z")
        );
    }

    /// Unparseable input → None (the caller keeps the original raw value and logs;
    /// such a row simply has no sortable-time guarantee).
    #[test]
    fn normalize_published_at_unparseable_is_none() {
        assert_eq!(normalize_published_at(""), None);
        assert_eq!(normalize_published_at("not a date"), None);
        assert_eq!(normalize_published_at("2026-13-45T99:99:99Z"), None);
        assert_eq!(normalize_published_at("Sep 2026"), None);
    }

    /// The canonical output must be exactly 20 chars (`YYYY-MM-DDTHH:MM:SSZ`) so
    /// that SQLite string ordering ≡ chronological ordering — assert length on a
    /// representative of each input family.
    #[test]
    fn normalize_published_at_is_fixed_20_char_shape() {
        for raw in [
            "Wed, 02 Sep 2026 12:00:00 GMT",
            "Wed, 02 Sep 2026 12:00:00 -0500",
            "2026-09-02T12:00:00Z",
            "2026-09-02T12:00:00+05:30",
        ] {
            let out = normalize_published_at(raw).expect("sample should parse");
            assert_eq!(out.len(), 20, "canonical form must be 20 chars: {out}");
            assert!(out.ends_with('Z'), "canonical form must end with Z: {out}");
            assert!(out.starts_with("2026-09-02T"));
        }
    }
}


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
            u.set_fragment(None);
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

/// Lexical URL guard for outbound feed GETs, shared by both pipelines.
///
/// This is a cheap lexical-layer defense, NOT a complete SSRF prevention
/// mechanism: a hostname that resolves to a private/loopback address is not
/// visible here (DNS rebinding is not covered), and the Workers platform has
/// its own outbound network boundary. It exists to reject the obvious mistakes
/// before any bytes are requested: non-http(s) schemes, no host, literal
/// local/internal names, and literal private/special-use IPs (including the
/// cloud-metadata endpoint 169.254.169.254).
///
/// Namespace rules stay deliberately narrow — `localhost`/`.localhost`/
/// `.local`/`.internal` only — rather than maintaining an ever-growing internal
/// domain blacklist.
pub(crate) fn is_safe_fetch_url(url: &url::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let lower = domain.to_ascii_lowercase();
            !(lower == "localhost"
                || lower.ends_with(".localhost")
                || lower.ends_with(".local")
                || lower.ends_with(".internal"))
        }
        Some(url::Host::Ipv4(addr)) => !is_unsafe_ipv4(&addr),
        Some(url::Host::Ipv6(addr)) => !is_unsafe_ipv6(&addr),
        None => false, // no host at all
    }
}

fn is_unsafe_ipv4(addr: &std::net::Ipv4Addr) -> bool {
    let o = addr.octets();
    // 0.0.0.0/8 (unspecified / this-network).
    if o[0] == 0 {
        return true;
    }
    // 127.0.0.0/8 loopback.
    if o[0] == 127 {
        return true;
    }
    // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 private.
    if o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168) {
        return true;
    }
    // 169.254.0.0/16 link-local (covers the metadata endpoint).
    if o[0] == 169 && o[1] == 254 {
        return true;
    }
    // 100.64.0.0/10 shared address space (CGNAT).
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return true;
    }
    // 224.0.0.0/4 multicast.
    if (224..=239).contains(&o[0]) {
        return true;
    }
    // 240.0.0.0/4 reserved (includes 255.255.255.255 broadcast).
    if o[0] >= 240 {
        return true;
    }
    // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 documentation.
    if (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
    {
        return true;
    }
    // 198.18.0.0/15 benchmarking.
    if o[0] == 198 && (18..=19).contains(&o[1]) {
        return true;
    }
    false
}

fn is_unsafe_ipv6(addr: &std::net::Ipv6Addr) -> bool {
    // IPv4-mapped (::ffff:a.b.c.d) — evaluate the embedded IPv4.
    if let Some(v4) = addr.to_ipv4_mapped() {
        return is_unsafe_ipv4(&v4);
    }
    let seg = addr.segments();
    // :: (unspecified) and ::1 (loopback).
    if seg.iter().all(|&s| s == 0) || (seg[..7].iter().all(|&s| s == 0) && seg[7] == 1) {
        return true;
    }
    // ff00::/8 multicast.
    if seg[0] & 0xff00 == 0xff00 {
        return true;
    }
    // fe80::/10 link-local.
    if seg[0] & 0xffc0 == 0xfe80 {
        return true;
    }
    // fc00::/7 unique-local (ULA).
    if seg[0] & 0xfe00 == 0xfc00 {
        return true;
    }
    // 2001:db8::/32 documentation.
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return true;
    }
    false
}

/// Resolve a `Location` header against the URL it came from (absolute URL,
/// protocol-relative `//host/path`, or root/relative path) into a fetchable
/// URL. Returns `None` for an unusable target — empty, unjoinable, or not
/// http(s).
pub(crate) fn resolve_redirect(base: &url::Url, location: &str) -> Option<url::Url> {
    if location.is_empty() {
        return None;
    }
    let joined = base.join(location).ok()?;
    if matches!(joined.scheme(), "http" | "https") {
        Some(joined)
    } else {
        None
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

    /// Atom/RFC3339 fractional seconds are legal ISO — they collapse to whole
    /// seconds (the canonical shape has no sub-second field).
    #[test]
    fn normalize_published_at_iso_fractional_seconds_to_whole_seconds() {
        assert_eq!(
            normalize_published_at("2026-09-02T12:00:00.123Z").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("2026-09-02T12:00:00.999+00:00").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("2026-08-15T09:30:45.500+05:30").as_deref(),
            Some("2026-08-15T04:00:45Z")
        );
    }

    /// RFC2822 permits an unpadded 1-2 digit day-of-month, but hour/minute/second
    /// must be 2-digit. chrono enforces exactly that, so unpadded time fields
    /// fall through to None (caller keeps raw) rather than being guessed.
    #[test]
    fn normalize_published_at_rfc2822_day_unpadded_ok_time_padded_required() {
        assert_eq!(
            normalize_published_at("Wed, 2 Sep 2026 12:00:00 GMT").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("Wed, 2 Sep 2026 08:05:09 +0000").as_deref(),
            Some("2026-09-02T08:05:09Z")
        );
        // Unpadded hour or minute/second is not RFC2822-legal — rejected.
        assert_eq!(normalize_published_at("Wed, 2 Sep 2026 3:04:05 GMT"), None);
        assert_eq!(normalize_published_at("Wed, 02 Sep 2026 8:0:0 +0000"), None);
    }

    /// Surrounding whitespace is tolerated (feeds occasionally pad the element).
    #[test]
    fn normalize_published_at_trims_surrounding_whitespace() {
        assert_eq!(
            normalize_published_at("  Wed, 02 Sep 2026 12:00:00 GMT  ").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(
            normalize_published_at("\t2026-09-02T12:00:00Z\n").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
    }

    /// RFC822 `-0000` means "unknown local offset" — by convention treated as
    /// UTC, so it maps to the same canonical instant as `+0000`/`Z`.
    #[test]
    fn normalize_published_at_unknown_offset_minus0000_is_utc() {
        assert_eq!(
            normalize_published_at("Wed, 02 Sep 2026 12:00:00 -0000").as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
    }

    /// Incomplete / zoneless forms are NOT silently promoted to a guessed time:
    /// they fall through to None (caller keeps raw + logs).
    #[test]
    fn normalize_published_at_incomplete_or_zoneless_is_none() {
        assert_eq!(normalize_published_at("2026-09-02"), None); // date only
        assert_eq!(normalize_published_at("2026-09-02T12:00:00"), None); // no zone
        assert_eq!(normalize_published_at("Tue, 01 Sep 2026"), None); // no time
        assert_eq!(normalize_published_at("Yesterday"), None);
    }

    /// Idempotence across encodings: the same real instant expressed in RFC822
    /// GMT / +0000, and ISO Z / +00:00, must converge to ONE canonical string —
    /// the whole point of a sortable key.
    #[test]
    fn normalize_published_at_same_instant_across_encodings_converge() {
        let encodings = [
            "Wed, 02 Sep 2026 12:00:00 GMT",
            "Wed, 02 Sep 2026 12:00:00 +0000",
            "2026-09-02T12:00:00Z",
            "2026-09-02T12:00:00+00:00",
        ];
        let canonical = encodings
            .iter()
            .map(|raw| normalize_published_at(raw).expect("encoding should parse"))
            .collect::<Vec<_>>();
        for c in &canonical {
            assert_eq!(c, "2026-09-02T12:00:00Z", "all encodings must converge");
        }
        // And a non-UTC offset is shifted rather than echoed verbatim.
        assert_eq!(
            normalize_published_at("Wed, 02 Sep 2026 12:00:00 -0500").as_deref(),
            normalize_published_at("2026-09-02T12:00:00-05:00").as_deref(),
        );
        assert_eq!(
            normalize_published_at("2026-09-02T12:00:00-05:00").as_deref(),
            Some("2026-09-02T17:00:00Z")
        );
    }

    fn assert_safe(raw: &str) {
        let url = url::Url::parse(raw).unwrap_or_else(|e| panic!("parse {raw}: {e}"));
        assert!(
            is_safe_fetch_url(&url),
            "{raw} should be fetchable under the lexical guard"
        );
    }

    fn assert_unsafe(raw: &str) {
        let url = url::Url::parse(raw).unwrap_or_else(|e| panic!("parse {raw}: {e}"));
        assert!(
            !is_safe_fetch_url(&url),
            "{raw} should be rejected by the lexical guard"
        );
    }

    /// Ordinary public hosts and non-private IPs pass the guard.
    #[test]
    fn is_safe_fetch_url_accepts_public_targets() {
        assert_safe("https://example.com");
        assert_safe("https://example.com:443/feed");
        assert_safe("http://example.com/news/rss.xml");
        // Public-ish IPv4 that happen to fall outside every rejected range
        // (172.32.x is above 172.31, 100.128.x above 100.127).
        assert_safe("http://172.32.0.1");
        assert_safe("http://100.128.0.1");
        // Public IPv6 (Cloudflare 1.1.1.1 over IPv6).
        assert_safe("http://[2606:4700::1111]");
    }

    /// Loopback is rejected in both families, including the IPv4-mapped form.
    #[test]
    fn is_safe_fetch_url_rejects_loopback() {
        assert_unsafe("http://127.0.0.1");
        assert_unsafe("http://127.0.0.2");
        assert_unsafe("http://[::1]");
        assert_unsafe("http://[::ffff:127.0.0.1]");
    }

    #[test]
    fn is_safe_fetch_url_rejects_private_ranges() {
        assert_unsafe("http://10.0.0.1");
        assert_unsafe("http://172.16.0.1");
        assert_unsafe("http://172.31.255.255"); // top of 172.16/12
        assert_unsafe("http://192.168.1.1");
        assert_unsafe("http://[fc00::1]"); // ULA
    }

    #[test]
    fn is_safe_fetch_url_rejects_link_local_and_metadata() {
        assert_unsafe("http://169.254.169.254"); // cloud metadata endpoint
        assert_unsafe("http://169.254.0.1");
        assert_unsafe("http://[fe80::1]");
    }

    /// 100.64.0.0/10 shared address space (CGNAT) is not publicly routable.
    #[test]
    fn is_safe_fetch_url_rejects_shared_cgnat() {
        assert_unsafe("http://100.64.0.1");
        assert_unsafe("http://100.127.255.255");
    }

    #[test]
    fn is_safe_fetch_url_rejects_special_use_ranges() {
        assert_unsafe("http://0.0.0.0"); // unspecified
        assert_unsafe("http://224.0.0.1"); // multicast
        assert_unsafe("http://255.255.255.255"); // broadcast / reserved
        assert_unsafe("http://192.0.2.1"); // documentation
        assert_unsafe("http://198.51.100.1"); // documentation
        assert_unsafe("http://203.0.113.1"); // documentation
        assert_unsafe("http://198.18.0.1"); // benchmarking
        assert_unsafe("http://[::]"); // IPv6 unspecified
        assert_unsafe("http://[2001:db8::1]"); // IPv6 documentation
        assert_unsafe("http://[ff02::1]"); // IPv6 multicast
    }

    /// Local/internal naming is rejected lexically; genuinely public DNS names
    /// are left for DNS resolution (a resolution-time check is out of scope).
    #[test]
    fn is_safe_fetch_url_rejects_internal_names() {
        assert_unsafe("http://localhost");
        assert_unsafe("http://localhost:8080/feed");
        assert_unsafe("http://foo.localhost");
        assert_unsafe("http://host.internal");
        assert_unsafe("http://printer.local");
    }

    #[test]
    fn is_safe_fetch_url_rejects_non_http_schemes_and_hostless() {
        assert_unsafe("ftp://example.com/file.xml");
        assert_unsafe("file:///etc/passwd");
        assert_unsafe("mailto:user@example.com");
    }

    /// Location resolution covers absolute, root-relative, protocol-relative and
    /// path-relative forms, and refuses anything that is not http(s).
    #[test]
    fn resolve_redirect_resolves_and_filters_targets() {
        let base = url::Url::parse("https://example.com/dir/feed.xml").unwrap();

        // Absolute URL.
        let abs = resolve_redirect(&base, "https://cdn.example.net/rss.xml").unwrap();
        assert_eq!(abs.as_str(), "https://cdn.example.net/rss.xml");

        // Root-relative.
        let root = resolve_redirect(&base, "/rss").unwrap();
        assert_eq!(root.as_str(), "https://example.com/rss");

        // Protocol-relative.
        let proto = resolve_redirect(&base, "//other.example.com/x").unwrap();
        assert_eq!(proto.as_str(), "https://other.example.com/x");

        // Path-relative with dot-segments resolved.
        let rel = resolve_redirect(&base, "../x").unwrap();
        assert_eq!(rel.as_str(), "https://example.com/x");

        // Query-only keeps the path.
        let q = resolve_redirect(&base, "?page=2").unwrap();
        assert_eq!(q.as_str(), "https://example.com/dir/feed.xml?page=2");

        // Non-http(s) targets are refused (this is what keeps a manual redirect
        // loop from following a `Location: file:///…`).
        assert!(resolve_redirect(&base, "ftp://bad.example.com/f").is_none());
        assert!(resolve_redirect(&base, "file:///etc/passwd").is_none());

        // Empty Location is terminal (no target).
        assert!(resolve_redirect(&base, "").is_none());
    }
}


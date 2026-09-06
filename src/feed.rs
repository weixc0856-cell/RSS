use crate::types::{Article, Feed};
use quick_xml::events::Event;
use quick_xml::Reader;
use url::Url;
use worker::d1::D1Type;
use worker::{
    console_error, console_log, Error, Env, Fetch, Headers, Method, Request, RequestInit,
    RequestRedirect, Result,
};

/// Cap per-fetch inserts so a single Worker invocation stays within
/// per-request limits (e.g. subrequests / D1 API calls on the free plan).
const MAX_ARTICLES_PER_FETCH: usize = 25;

/// Max redirect hops `fetch_feed_document` will follow manually.
const MAX_REDIRECTS: usize = 5;
/// Post-buffering validation limit, NOT a hard memory cap: worker-rs 0.8.5 body
/// reads are fully buffered (`Response::text()` buffers before any length can be
/// checked), so this bounds what reaches the XML parser / D1 pipeline, not peak
/// read memory.
const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;
/// Response-time deadline per HTTP hop — a deadline, not a guaranteed
/// cancellation of the underlying outbound request (see `race_timeout`).
const FETCH_TIMEOUT_SECS: u64 = 20;

pub struct FeedParser;

impl FeedParser {
    pub async fn fetch_feed(url: &str) -> Result<Vec<Article>> {
        let parsed = Url::parse(url).map_err(|error| Error::RustError(error.to_string()))?;
        let fetched = fetch_feed_document(&parsed, &[]).await?;
        if !(200..300).contains(&fetched.status) {
            return Err(Error::RustError(format!("feed returned HTTP {}", fetched.status)));
        }

        parse_document(&fetched.body, 0)
    }

    pub fn parse_rss(content: &str, feed_id: i32) -> Result<Vec<Article>> {
        parse_document(content, feed_id)
    }

    pub fn parse_atom(content: &str, feed_id: i32) -> Result<Vec<Article>> {
        parse_document(content, feed_id)
    }

    pub fn generate_article_hash(title: &str, link: &str) -> String {
        format!("{:x}", md5::compute(format!("{}{}", title, link)))
    }
}

/// D1 bindings do not accept `undefined`; nullable text columns must be bound
/// as an explicit SQL `NULL` (`D1Type::Null`) or a string.
fn d1_text_or_null(value: &Option<String>) -> D1Type<'_> {
    match value {
        Some(text) => D1Type::Text(text),
        None => D1Type::Null,
    }
}

/// Result of a hardened outbound feed GET.
///
/// `body` is populated only for successful 2xx responses; non-2xx responses may
/// carry an empty body (a 304 is always empty and must never be parsed as if it
/// were an empty feed).
pub(crate) struct FetchedFeed {
    pub(crate) status: u16,
    pub(crate) etag: Option<String>,
    pub(crate) last_modified: Option<String>,
    pub(crate) body: String,
}

/// One outbound GET for a feed with a browser-like `User-Agent` (a number of
/// publishers use it to decide whether to serve RSS or block bots). Redirects
/// are NOT followed by the runtime (`RequestRedirect::Manual`): each hop is
/// re-validated against the SSRF guard by `fetch_feed_document`. `extra`
/// carries the conditional-GET validators and is re-sent on every hop — the
/// same effect runtime "follow" mode had on them.
async fn send_once(url: &Url, extra: &[(&str, &str)]) -> Result<worker::Response> {
    let mut headers = Headers::new();
    headers.set(
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
    )?;
    for (name, value) in extra {
        let _ = headers.set(name, value);
    }

    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    init.with_redirect(RequestRedirect::Manual);
    let request = Request::new_with_init(url.as_str(), &init)?;
    Fetch::Request(request).send().await
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn response_header(response: &worker::Response, name: &str) -> Option<String> {
    response.headers().get(name).ok().flatten()
}

/// Shared outbound feed-GET core used by the `feeds` pipeline. Transport
/// hardening only — the HTTP status mapping, 304 semantics and error messages
/// the callers rely on are unchanged; this adds a timeout, a response-size cap,
/// a lexical SSRF guard and a redirect cap.
///
/// Redirect hops are followed manually and EVERY hop (starting URL included) is
/// re-checked against the SSRF guard — the real risk is
/// `https://trusted/feed -> 302 Location: http://127.0.0.1`, not the original
/// URL. The response body is read at most once, and only after the final
/// response has been classified as a successful 2xx response; 304 and other
/// non-2xx responses are returned with an empty body for the caller to act on.
pub(crate) async fn fetch_feed_document(
    url: &Url,
    extra: &[(&str, &str)],
) -> Result<FetchedFeed> {
    if !crate::utils::is_safe_fetch_url(url) {
        return Err(Error::RustError(format!("blocked unsafe fetch URL: {url}")));
    }

    let mut current = url.clone();
    let mut redirects = 0usize;

    loop {
        let mut response = race_timeout(send_once(&current, extra), FETCH_TIMEOUT_SECS).await?;
        let status = response.status_code();

        if is_redirect_status(status) {
            let location = response_header(&response, "location").filter(|v| !v.is_empty());
            let next =
                match location.and_then(|loc| crate::utils::resolve_redirect(&current, &loc)) {
                    Some(next) => next,
                    None => {
                        // Redirect without a usable Location is a terminal
                        // non-2xx response and follows the existing HTTP error
                        // path (the caller sees a non-2xx status, empty body).
                        return Ok(FetchedFeed {
                            status,
                            etag: None,
                            last_modified: None,
                            body: String::new(),
                        });
                    }
                };
            if redirects >= MAX_REDIRECTS {
                return Err(Error::RustError(format!(
                    "feed redirect limit exceeded ({MAX_REDIRECTS}) from {url}"
                )));
            }
            if !crate::utils::is_safe_fetch_url(&next) {
                return Err(Error::RustError(format!(
                    "blocked unsafe redirect target: {next}"
                )));
            }
            redirects += 1;
            console_log!(
                "[feed] redirect {status} -> {next} (hop {redirects}/{MAX_REDIRECTS})"
            );
            current = next;
            continue;
        }

        let etag = response_header(&response, "etag").filter(|v| !v.is_empty());
        let last_modified = response_header(&response, "last-modified").filter(|v| !v.is_empty());

        // Content-Length pre-check (advisory — the authoritative cap is the
        // post-read `body.len()` check below).
        if let Some(raw) = response_header(&response, "content-length") {
            if let Ok(len) = raw.parse::<usize>() {
                if len > MAX_FEED_BYTES {
                    return Err(Error::RustError(format!(
                        "feed response too large: Content-Length {len} exceeds {MAX_FEED_BYTES} bytes"
                    )));
                }
            }
        }

        if !(200..300).contains(&status) {
            // 304 / other non-2xx: never read the body.
            return Ok(FetchedFeed {
                status,
                etag,
                last_modified,
                body: String::new(),
            });
        }

        let body = race_timeout(response.text(), FETCH_TIMEOUT_SECS).await?;
        if body.len() > MAX_FEED_BYTES {
            return Err(Error::RustError(format!(
                "feed response too large: {} bytes exceeds {MAX_FEED_BYTES} bytes",
                body.len()
            )));
        }
        return Ok(FetchedFeed {
            status,
            etag,
            last_modified,
            body,
        });
    }
}

/// Race `fut` against a `secs`-second deadline.
///
/// A response-time deadline, NOT a guaranteed cancellation of the underlying
/// outbound request: on the wasm Worker the in-flight fetch future is dropped
/// (its JS promise released) rather than hard-aborted, because worker-rs 0.8.5
/// `RequestInit` exposes no signal/timeout field. `worker::Delay` is `!Unpin`,
/// so both race branches must be pinned. On the native host the timer is
/// compiled out (no real outbound fetch path is exercised under `cargo test`).
async fn race_timeout<F, T>(fut: F, secs: u64) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    #[cfg(target_arch = "wasm32")]
    {
        use futures_util::future::{select, Either};
        let work = Box::pin(fut);
        let timer = Box::pin(worker::Delay::from(std::time::Duration::from_secs(secs)));
        match select(work, timer).await {
            Either::Left((outcome, _timer)) => outcome,
            Either::Right(((), _work)) => Err(Error::RustError(format!(
                "feed fetch timed out after {secs}s"
            ))),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = secs;
        fut.await
    }
}

const DEFAULT_FETCH_INTERVAL_MINUTES: i64 = 15;
const MAX_BACKOFF_MINUTES: i64 = 24 * 60;

fn backoff_minutes(interval: i64, consecutive_failures: i64) -> i64 {
    let shift = consecutive_failures.clamp(0, 6) as u32;
    let base = interval.max(5);
    (base * 2i64.pow(shift)).min(MAX_BACKOFF_MINUTES)
}

pub async fn fetch_feed(url: &str, env: &Env) -> Result<usize> {
    let db = env.d1("rss_db")?;
    let row = db
        .prepare(
            "SELECT f.id, f.url, f.fetch_interval_minutes, f.consecutive_failures, f.etag, f.last_modified
             FROM feeds f
             WHERE f.url = ?1
               AND EXISTS (SELECT 1 FROM subscriptions s WHERE s.feed_id = f.id)",
        )
        .bind(&[url.into()])?
        .first::<serde_json::Value>(None)
        .await?
        .ok_or_else(|| Error::RustError("feed not found or has no subscribers".to_string()))?;

    let feed_id = row["id"].as_i64().unwrap_or(0) as i32;
    if feed_id <= 0 {
        return Err(Error::RustError("feed id missing".to_string()));
    }
    let interval = row["fetch_interval_minutes"]
        .as_i64()
        .unwrap_or(DEFAULT_FETCH_INTERVAL_MINUTES);
    let consecutive_failures = row["consecutive_failures"].as_i64().unwrap_or(0);
    let etag = row["etag"].as_str().map(|s| s.to_string());
    let last_modified = row["last_modified"].as_str().map(|s| s.to_string());

    console_log!("[feed] fetch start feed_id={} url={}", feed_id, url);

    // Conditional GET when the origin previously gave us validators.
    let mut conditional = Vec::new();
    for (name, value) in [
        ("If-None-Match", etag.as_deref()),
        ("If-Modified-Since", last_modified.as_deref()),
    ] {
        if let Some(value) = value {
            if !value.is_empty() {
                conditional.push((name, value));
            }
        }
    }

    let fetched = fetch_feed_document(
        &Url::parse(url).map_err(|e| Error::RustError(e.to_string()))?,
        &conditional,
    )
    .await?;
    let status = fetched.status as i32;

    // 304 Not Modified => content unchanged; counts as success (fresh), no
    // parsing. Re-store the validators we just sent: the origin did not give us
    // new ones, so keep the old etag/last_modified or the next fetch degrades to
    // an unconditional full GET.
    if status == 304 {
        console_log!("[feed] 304 not modified feed_id={}", feed_id);
        mark_success(
            &db,
            feed_id,
            304,
            interval,
            etag.clone(),
            last_modified.clone(),
        )
        .await?;
        return Ok(0);
    }

    if !(200..300).contains(&status) {
        let message = format!("feed returned HTTP {status}");
        console_error!("[feed] http error feed_id={} {message}", feed_id);
        mark_failure(&db, feed_id, status, Some(message.clone()), interval, consecutive_failures)
            .await?;
        return Err(Error::RustError(message));
    }

    let new_etag = fetched.etag.filter(|v| !v.is_empty());
    let new_last_modified = fetched.last_modified.filter(|v| !v.is_empty());

    let before = count_articles(&db, feed_id).await?;
    let content = fetched.body;
    let articles = match parse_document(&content, feed_id) {
        Ok(articles) => articles,
        Err(error) => {
            console_error!("[feed] parse error feed_id={}: {}", feed_id, error);
            mark_failure(
                &db,
                feed_id,
                status,
                Some(error.to_string()),
                interval,
                consecutive_failures,
            )
            .await?;
            return Err(error);
        }
    };

    // Execution gate, re-check: the fetch above spent seconds outbound, during
    // which the last subscriber may have unsubscribed and pruned this feed +
    // its articles. Skip persisting entirely (Ok(0), no mark_success/failure on
    // the now-deleted feed row) rather than resurrect orphan articles.
    let still_subscribed = db
        .prepare("SELECT 1 FROM subscriptions WHERE feed_id = ?1 LIMIT 1")
        .bind(&[feed_id.into()])?
        .first::<serde_json::Value>(None)
        .await?
        .is_some();
    if !still_subscribed {
        console_log!(
            "[feed] skip persist feed_id={}: no subscribers remain",
            feed_id
        );
        return Ok(0);
    }

    for article in articles.into_iter().take(MAX_ARTICLES_PER_FETCH) {
        let args = [
            D1Type::Integer(article.feed_id),
            D1Type::Text(&article.title),
            D1Type::Text(&article.link),
            D1Type::Text(&article.guid),
            d1_text_or_null(&article.summary),
            d1_text_or_null(&article.content),
            d1_text_or_null(&article.published_at),
            D1Type::Text(&article.hash),
        ];
        db.prepare(
            "INSERT OR IGNORE INTO articles
             (feed_id, title, link, guid, summary, content, published_at, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind_refs(args.iter())?
        .run()
        .await?;
    }
    let after = count_articles(&db, feed_id).await?;
    let inserted = after.saturating_sub(before);

    console_log!(
        "[feed] fetch ok feed_id={} parsed_articles={} inserted={}",
        feed_id,
        MAX_ARTICLES_PER_FETCH,
        inserted
    );
    mark_success(&db, feed_id, status, interval, new_etag, new_last_modified).await?;
    Ok(inserted as usize)
}

async fn count_articles(db: &worker::D1Database, feed_id: i32) -> Result<i64> {
    let row = db
        .prepare("SELECT COUNT(*) AS c FROM articles WHERE feed_id = ?1")
        .bind(&[feed_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    Ok(row.and_then(|v| v["c"].as_i64()).unwrap_or(0))
}


/// Feed fetch succeeded: refresh health fields and schedule the next window.
async fn mark_success(
    db: &worker::D1Database,
    feed_id: i32,
    http_status: i32,
    interval: i64,
    etag: Option<String>,
    last_modified: Option<String>,
) -> Result<()> {
    let next = crate::utils::sqlite_now_plus_minutes(interval.max(DEFAULT_FETCH_INTERVAL_MINUTES));
    let etag_arg = etag.as_deref().map(D1Type::Text).unwrap_or(D1Type::Null);
    let lm_arg = last_modified.as_deref().map(D1Type::Text).unwrap_or(D1Type::Null);
    let args = [
        D1Type::Integer(http_status),
        etag_arg,
        lm_arg,
        D1Type::Text(&next),
        D1Type::Integer(feed_id),
    ];
    db.prepare(
        "UPDATE feeds SET
            status = 'active',
            error_message = NULL,
            last_fetched_at = datetime('now'),
            last_success_at = datetime('now'),
            last_http_status = ?1,
            consecutive_failures = 0,
            etag = ?2,
            last_modified = ?3,
            next_fetch_at = ?4,
            updated_at = datetime('now')
         WHERE id = ?5",
    )
    .bind_refs(args.iter())?
    .run()
    .await?;
    Ok(())
}

/// Feed fetch failed: record the failure and back off exponentially so a broken
/// origin (e.g. HTTP 503) is not hammered every scheduler cycle.
async fn mark_failure(
    db: &worker::D1Database,
    feed_id: i32,
    http_status: i32,
    error_message: Option<String>,
    interval: i64,
    consecutive_failures: i64,
) -> Result<()> {
    let failures = consecutive_failures + 1;
    let next = crate::utils::sqlite_now_plus_minutes(backoff_minutes(interval, failures));
    let args = [
        D1Type::Integer(http_status),
        d1_text_or_null(&error_message),
        D1Type::Integer(failures as i32),
        D1Type::Text(&next),
        D1Type::Integer(feed_id),
    ];
    db.prepare(
        "UPDATE feeds SET
            status = 'error',
            last_fetched_at = datetime('now'),
            last_failure_at = datetime('now'),
            last_http_status = ?1,
            error_message = ?2,
            consecutive_failures = ?3,
            next_fetch_at = ?4,
            updated_at = datetime('now')
         WHERE id = ?5",
    )
    .bind_refs(args.iter())?
    .run()
    .await?;
    Ok(())
}

fn parse_document(content: &str, feed_id: i32) -> Result<Vec<Article>> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);
    let mut articles = Vec::new();
    let mut current: Option<ParsedArticle> = None;
    let mut field = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let event_name = event.name();
                let name = local_name(event_name.as_ref());
                if name == "item" || name == "entry" {
                    current = Some(ParsedArticle::default());
                } else if current.is_some() {
                    field = name.to_string();
                    if name == "link" {
                        if let Some(article) = current.as_mut() {
                            article.link = attribute(&event, b"href");
                        }
                    }
                }
            }
            Ok(Event::Empty(event)) => {
                if current.is_some() && local_name(event.name().as_ref()) == "link" {
                    if let Some(article) = current.as_mut() {
                        article.link = attribute(&event, b"href");
                    }
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(article) = current.as_mut() {
                    let value = text
                        .unescape()
                        .map_err(|error| Error::RustError(error.to_string()))?
                        .into_owned();
                    set_field(article, &field, value);
                }
            }
            Ok(Event::CData(cdata)) => {
                if let Some(article) = current.as_mut() {
                    let value = cdata
                        .decode()
                        .map_err(|error| Error::RustError(error.to_string()))?
                        .into_owned();
                    set_field(article, &field, value);
                }
            }
            Ok(Event::End(event)) => {
                let event_name = event.name();
                let name = local_name(event_name.as_ref());
                if (name == "item" || name == "entry") && current.is_some() {
                    let parsed = current.take().unwrap();
                    if !parsed.title.is_empty() && !parsed.link.is_empty() {
                        let guid = if parsed.guid.is_empty() {
                            parsed.link.clone()
                        } else {
                            parsed.guid.clone()
                        };
                        let hash = FeedParser::generate_article_hash(&parsed.title, &parsed.link);
                        // Single choke point where feed-native published timestamps
                        // (RSS RFC822 text, Atom ISO) become the canonical sortable
                        // UTC ISO form `YYYY-MM-DDTHH:MM:SSZ`. `ORDER BY published_at
                        // DESC` / `MAX(published_at)` are chronological only while every
                        // row obeys that fixed shape — so bypassing this is a contract
                        // violation. Never lose data: an unparseable value is preserved
                        // verbatim and logged; the row then has no sortable-time
                        // guarantee.
                        let published_at = parsed.published_at.as_deref().map(|raw| {
                            crate::utils::normalize_published_at(raw).unwrap_or_else(|| {
                                log_normalize_failure(feed_id, &parsed.title);
                                raw.to_string()
                            })
                        });
                        articles.push(Article {
                            id: 0,
                            feed_id,
                            title: parsed.title,
                            link: parsed.link,
                            guid,
                            summary: parsed.summary,
                            content: parsed.content,
                            published_at,
                            hash,
                        });
                    }
                    field.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(Error::RustError(error.to_string())),
            _ => {}
        }
    }

    Ok(articles)
}

#[derive(Default)]
struct ParsedArticle {
    title: String,
    link: String,
    guid: String,
    summary: Option<String>,
    content: Option<String>,
    published_at: Option<String>,
}

fn set_field(article: &mut ParsedArticle, field: &str, value: String) {
    match field {
        "title" => article.title = value,
        "link" if article.link.is_empty() => article.link = value,
        "guid" | "id" => article.guid = value,
        "description" | "summary" => article.summary = Some(value),
        "encoded" | "content" => article.content = Some(value),
        "pubDate" | "published" | "updated" => article.published_at = Some(value),
        _ => {}
    }
}

/// Failure log for the published_at choke point. On wasm (the deployed Worker)
/// this goes through worker's `console_log!`; under `cargo test` (host) that
/// symbol is a wasm-import shim that must not be invoked at runtime, so log to
/// stderr instead. Keeps the choke point pure and unit-testable.
fn log_normalize_failure(feed_id: i32, title: &str) {
    #[cfg(target_arch = "wasm32")]
    console_log!("[feed] failed to normalize published_at feed_id={} title={}", feed_id, title);
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("[feed] failed to normalize published_at feed_id={} title={}", feed_id, title);
}

fn local_name(name: &[u8]) -> &str {
    let name = std::str::from_utf8(name).unwrap_or("");
    name.rsplit(':').next().unwrap_or(name)
}

fn attribute(event: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> String {
    event
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.as_ref() == key)
        .and_then(|attribute| attribute.unescape_value().ok())
        .map(|value| value.into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // RSS 2.0 fixture, deliberately pretty-printed (whitespace + namespaces)
    // to exercise realistic feeds.
    const RSS_SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>Example Blog</title>
    <link>https://example.com/</link>
    <description>Example blog channel</description>
    <item>
      <title>First Post &amp; News</title>
      <link>https://example.com/first</link>
      <guid isPermaLink="false">post-1</guid>
      <description>A &lt;b&gt;short&lt;/b&gt; summary &amp; more</description>
      <content:encoded><![CDATA[<p>Full content here &amp; raw CDATA</p>]]></content:encoded>
      <pubDate>Tue, 01 Sep 2026 10:20:30 GMT</pubDate>
    </item>
    <item>
      <title>Second Post</title>
      <link>https://example.com/second</link>
      <description>Second summary</description>
      <pubDate>Wed, 02 Sep 2026 08:00:00 GMT</pubDate>
    </item>
  </channel>
</rss>
"#;

    // Atom fixture: links come from href attributes, entries use id/published.
    const ATOM_SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Example Atom Feed</title>
  <id>urn:uuid:feed-root</id>
  <updated>2026-09-02T12:00:00Z</updated>
  <link href="https://example.com/atom"/>
  <entry>
    <title>Atom Entry One</title>
    <id>urn:uuid:entry-1</id>
    <link href="https://example.com/entry-1?page=1&amp;lang=en"/>
    <summary>Atom summary one</summary>
    <published>2026-08-15T09:30:00Z</published>
  </entry>
  <entry>
    <title>Atom Entry Two</title>
    <id>urn:uuid:entry-2</id>
    <link rel="alternate" type="text/html" href="https://example.com/entry-2"/>
    <content type="html">Atom &lt;b&gt;content&lt;/b&gt; two</content>
    <updated>2026-09-01T07:00:00Z</updated>
  </entry>
</feed>
"#;

    #[test]
    fn parse_rss_returns_all_items_with_fields() {
        let articles = FeedParser::parse_rss(RSS_SAMPLE, 42).expect("valid RSS should parse");

        assert_eq!(articles.len(), 2, "expected two RSS items");

        let first = &articles[0];
        // Text is XML-unescaped.
        assert_eq!(first.title, "First Post & News");
        assert_eq!(first.link, "https://example.com/first");
        assert_eq!(first.guid, "post-1");
        assert_eq!(first.summary.as_deref(), Some("A <b>short</b> summary & more"));
        // CDATA content is captured verbatim (only encoding, not XML, is decoded).
        assert_eq!(
            first.content.as_deref(),
            Some("<p>Full content here &amp; raw CDATA</p>")
        );
        assert_eq!(
            first.published_at.as_deref(),
            // RSS pubDate is normalized to canonical UTC ISO at the write choke
            // point — NOT kept as feed-native RFC822 text.
            Some("2026-09-01T10:20:30Z")
        );
        assert_eq!(first.id, 0);
        assert_eq!(first.feed_id, 42);

        let second = &articles[1];
        assert_eq!(second.title, "Second Post");
        assert_eq!(second.link, "https://example.com/second");
        // RSS item without <guid> falls back to <link>.
        assert_eq!(second.guid, second.link);
        assert_eq!(second.content, None);
        assert_eq!(
            second.published_at.as_deref(),
            Some("2026-09-02T08:00:00Z")
        );
    }

    /// Write-contract test: whatever the feed-native shape, the stored
    /// `published_at` must be canonical `YYYY-MM-DDTHH:MM:SSZ` (string sort ==
    /// time sort). An offset pubDate is shifted to UTC, so the row's true time
    /// ordering survives a lexicographic `ORDER BY published_at DESC`.
    #[test]
    fn parse_rss_normalizes_pubdate_to_canonical_utc_iso() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <item>
    <title>Earlier in real time (12:00 +05:30 = 06:30Z)</title>
    <link>https://example.com/a</link>
    <pubDate>Wed, 02 Sep 2026 12:00:00 +0530</pubDate>
  </item>
  <item>
    <title>Later in real time (12:00 GMT)</title>
    <link>https://example.com/b</link>
    <pubDate>Wed, 02 Sep 2026 12:00:00 GMT</pubDate>
  </item>
  <item>
    <title>No pubDate at all — published_at stays None</title>
    <link>https://example.com/c</link>
  </item>
</channel></rss>"#;
        let articles = FeedParser::parse_rss(xml, 1).expect("parse");
        assert_eq!(
            articles[0].published_at.as_deref(),
            Some("2026-09-02T06:30:00Z")
        );
        assert_eq!(
            articles[1].published_at.as_deref(),
            Some("2026-09-02T12:00:00Z")
        );
        assert_eq!(articles[2].published_at, None);
        // 06:30Z happened before 12:00Z, and the canonical strings agree.
        assert!(
            articles[0].published_at < articles[1].published_at,
            "canonical lexicographic order must match real time order"
        );
    }

    /// Write-contract, None path: an unparseable pubDate is preserved verbatim
    /// (never dropped, never guessed) — the article still stores, but the row
    /// carries no sortable-time guarantee.
    #[test]
    fn parse_rss_keeps_unparseable_pubdate_verbatim() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <item>
    <title>Odd date</title>
    <link>https://example.com/odd</link>
    <pubDate>not a real date</pubDate>
  </item>
</channel></rss>"#;
        let articles = FeedParser::parse_rss(xml, 1).expect("parse");
        assert_eq!(articles.len(), 1);
        assert_eq!(
            articles[0].published_at.as_deref(),
            Some("not a real date"),
            "unparseable pubDate must be preserved, not dropped or guessed"
        );
    }

    /// Write-contract for Atom: fractional-second <published> and offset
    /// <updated> collapse to the same whole-second canonical form RSS gets.
    #[test]
    fn parse_atom_normalizes_fractional_and_offset_timestamps() {
        let xml = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Fractional Atom</title>
  <link href="https://example.com/atom"/>
  <entry>
    <title>Fractional published</title>
    <id>urn:uuid:f-1</id>
    <link href="https://example.com/f1"/>
    <published>2026-09-02T08:30:15.250Z</published>
  </entry>
  <entry>
    <title>Offset updated</title>
    <id>urn:uuid:f-2</id>
    <link href="https://example.com/f2"/>
    <updated>2026-09-02T14:30:00+02:00</updated>
  </entry>
</feed>"#;
        let articles = FeedParser::parse_atom(xml, 9).expect("parse");
        assert_eq!(articles.len(), 2);
        assert_eq!(
            articles[0].published_at.as_deref(),
            Some("2026-09-02T08:30:15Z")
        );
        assert_eq!(
            articles[1].published_at.as_deref(),
            Some("2026-09-02T12:30:00Z")
        );
    }

    #[test]
    fn parse_rss_sets_dedupe_hash_per_item() {
        let articles = FeedParser::parse_rss(RSS_SAMPLE, 42).expect("parse");
        for article in &articles {
            let expected = FeedParser::generate_article_hash(&article.title, &article.link);
            assert_eq!(article.hash, expected);
            assert_eq!(article.hash.len(), 32, "md5 hex must be 32 chars");
            assert!(article.hash.chars().all(|c| c.is_ascii_hexdigit()));
        }
        assert_ne!(articles[0].hash, articles[1].hash, "different items -> different hashes");
    }

    #[test]
    fn parse_rss_skips_items_missing_title_or_link() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <item>
    <title>Complete Item</title>
    <link>https://example.com/ok</link>
    <description>kept</description>
  </item>
  <item>
    <title>Title only, no link</title>
  </item>
  <item>
    <link>https://example.com/link-only</link>
  </item>
  <item>
    <description>Neither title nor link</description>
  </item>
</channel></rss>"#;

        let articles = FeedParser::parse_rss(xml, 1).expect("parse");
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "Complete Item");
        assert_eq!(articles[0].link, "https://example.com/ok");
    }

    #[test]
    fn parse_atom_parses_entries_and_unescapes_href() {
        let articles = FeedParser::parse_atom(ATOM_SAMPLE, 7).expect("valid Atom should parse");

        assert_eq!(articles.len(), 2, "expected two Atom entries");

        let first = &articles[0];
        assert_eq!(first.title, "Atom Entry One");
        // Atom link is taken from the href attribute and &amp; is decoded.
        assert_eq!(first.link, "https://example.com/entry-1?page=1&lang=en");
        assert_eq!(first.guid, "urn:uuid:entry-1");
        assert_eq!(first.summary.as_deref(), Some("Atom summary one"));
        assert_eq!(first.published_at.as_deref(), Some("2026-08-15T09:30:00Z"));
        assert_eq!(first.content, None);
        assert_eq!(first.feed_id, 7);

        let second = &articles[1];
        assert_eq!(second.title, "Atom Entry Two");
        assert_eq!(second.guid, "urn:uuid:entry-2");
        assert_eq!(second.link, "https://example.com/entry-2");
        assert_eq!(second.summary, None);
        assert_eq!(second.content.as_deref(), Some("Atom <b>content</b> two"));
        // Feed-level <updated> must NOT leak into the entry; entry <updated> wins.
        assert_eq!(second.published_at.as_deref(), Some("2026-09-01T07:00:00Z"));
    }

    #[test]
    fn parse_rss_handles_cdata_and_escaped_text_consistently() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <item>
    <title>CDATA Item</title>
    <link>https://example.com/cdata</link>
    <description><![CDATA[raw <em>html</em> & text]]></description>
    <content:encoded><![CDATA[<div>block</div>]]></content:encoded>
  </item>
  <item>
    <title>Escaped Item</title>
    <link>https://example.com/escaped</link>
    <description>escaped &lt;em&gt;html&lt;/em&gt; &amp; text</description>
  </item>
</channel></rss>"#;

        let articles = FeedParser::parse_rss(xml, 3).expect("parse");
        assert_eq!(articles.len(), 2);

        // CDATA is not entity-unescaped (contents are already literal).
        assert_eq!(articles[0].summary.as_deref(), Some("raw <em>html</em> & text"));
        assert_eq!(articles[0].content.as_deref(), Some("<div>block</div>"));

        // Regular escaped text IS unescaped.
        assert_eq!(
            articles[1].summary.as_deref(),
            Some("escaped <em>html</em> & text")
        );
    }

    #[test]
    fn parse_rss_handles_numeric_character_references() {
        let xml = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <item>
    <title>&#65;mpersand &#38; friends</title>
    <link>https://example.com/numeric</link>
    <description>&#x41;&#x26;B</description>
  </item>
</channel></rss>"#;

        let articles = FeedParser::parse_rss(xml, 1).expect("parse");
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "Ampersand & friends");
        assert_eq!(articles[0].summary.as_deref(), Some("A&B"));
    }

    #[test]
    fn parse_rss_returns_error_on_malformed_xml() {
        // Mismatched closing tags are rejected by quick-xml (check_end_names).
        let malformed =
            r#"<rss version="2.0"><channel><item><title>Broken</item></channel></rss>"#;
        let result = FeedParser::parse_rss(malformed, 1);
        assert!(result.is_err(), "mismatched tags should produce an error");
    }

    #[test]
    fn parse_rss_empty_input_yields_no_articles() {
        assert_eq!(FeedParser::parse_rss("", 1).expect("empty is ok"), Vec::new());
        assert_eq!(
            FeedParser::parse_rss("not xml at all", 1).expect("plain text is ok"),
            Vec::new()
        );
        assert_eq!(
            FeedParser::parse_rss(
                r#"<rss version="2.0"><channel><title>x</title></channel></rss>"#,
                1
            )
            .expect("channel without items is ok"),
            Vec::new()
        );
    }

    #[test]
    fn article_hash_is_standard_md5_hex() {
        // md5("foobar") = 3858f62230ac3c915f300c664312c63f
        assert_eq!(
            FeedParser::generate_article_hash("foo", "bar"),
            "3858f62230ac3c915f300c664312c63f"
        );
        // Deterministic for identical input.
        assert_eq!(
            FeedParser::generate_article_hash("foo", "bar"),
            FeedParser::generate_article_hash("foo", "bar")
        );
        // Sensitive to either input component.
        assert_ne!(
            FeedParser::generate_article_hash("foo", "bar"),
            FeedParser::generate_article_hash("foo", "baz")
        );
        assert_ne!(
            FeedParser::generate_article_hash("foo", "bar"),
            FeedParser::generate_article_hash("fooo", "bar")
        );
    }

    #[test]
    fn local_name_strips_xml_namespace_prefixes() {
        assert_eq!(local_name(b"content:encoded"), "encoded");
        assert_eq!(local_name(b"dc:creator"), "creator");
        assert_eq!(local_name(b"item"), "item");
        assert_eq!(local_name(b"entry"), "entry");
        // Invalid UTF-8 gracefully degrades to "".
        assert_eq!(local_name(&[0xff, 0xfe]), "");
    }

    #[test]
    fn attribute_returns_requested_attr_and_unescapes() {
        let xml = r#"<item href="https://example.com/a&amp;b" missing="x">"#;
        let mut reader = Reader::from_str(xml);
        let event = reader.read_event().expect("read event");
        let Event::Start(start) = event else {
            panic!("expected Start event");
        };

        assert_eq!(attribute(&start, b"href"), "https://example.com/a&b");
        // Absent attributes yield the empty string.
        assert_eq!(attribute(&start, b"rel"), "");
    }

    #[test]
    fn attribute_matching_is_case_sensitive() {
        let xml = r#"<entry HREF="https://example.com/upper" href="https://example.com/lower">"#;
        let mut reader = Reader::from_str(xml);
        let event = reader.read_event().expect("read event");
        let Event::Start(start) = event else {
            panic!("expected Start event");
        };
        assert_eq!(attribute(&start, b"href"), "https://example.com/lower");
    }

    #[test]
    fn d1_text_or_null_encodes_none_as_null_and_some_as_text() {
        assert!(matches!(d1_text_or_null(&None), D1Type::Null));
        assert!(matches!(
            d1_text_or_null(&Some("hello".to_string())),
            D1Type::Text("hello")
        ));
    }

    /// Only the redirect statuses the manual loop follows (301/302/303/307/308)
    /// count as redirects. 300 is Multiple Choices (no automatic follow),
    /// 304 is a terminal Not Modified, and everything else is a normal outcome.
    #[test]
    fn is_redirect_status_classifies_status_codes() {
        for status in [301u16, 302, 303, 307, 308] {
            assert!(is_redirect_status(status), "{status} must be a redirect");
        }
        for status in [200u16, 201, 204, 300, 304, 400, 404, 410, 500, 503] {
            assert!(!is_redirect_status(status), "{status} must NOT be a redirect");
        }
    }

}


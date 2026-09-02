use crate::types::{Article, Feed};
use quick_xml::events::Event;
use quick_xml::Reader;
use url::Url;
use worker::d1::D1Type;
use worker::{Error, Env, Fetch, Headers, Method, Request, RequestInit, Result};

/// Cap per-fetch inserts so a single Worker invocation stays within
/// per-request limits (e.g. subrequests / D1 API calls on the free plan).
const MAX_ARTICLES_PER_FETCH: usize = 25;

pub struct FeedParser;

impl FeedParser {
    pub async fn fetch_feed(url: &str) -> Result<Vec<Article>> {
        let mut response = fetch_with_ua(&Url::parse(url).map_err(|error| Error::RustError(error.to_string()))?).await?;
        if !(200..300).contains(&response.status_code()) {
            return Err(Error::RustError(format!("feed returned HTTP {}", response.status_code())));
        }

        let content = response.text().await?;
        parse_document(&content, 0)
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

/// Send an outbound GET for a feed with a browser-like `User-Agent`, which a
/// number of publishers use to decide whether to serve RSS or block bots.
async fn fetch_with_ua(url: &Url) -> Result<worker::Response> {
    let mut headers = Headers::new();
    headers.set(
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
    )?;

    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let request = Request::new_with_init(url.as_str(), &init)?;
    Fetch::Request(request).send().await
}

pub async fn fetch_feed(url: &str, env: &Env) -> Result<()> {
    let db = env.d1("rss_db")?;
    let feed = db
        .prepare("SELECT id, url, title, site_url, favicon_url, last_fetched_at, status FROM feeds WHERE url = ?1")
        .bind(&[url.into()])?
        .first::<Feed>(None)
        .await?
        .ok_or_else(|| Error::RustError("feed not found".to_string()))?;

    let mut response = fetch_with_ua(&Url::parse(url).map_err(|error| Error::RustError(error.to_string()))?).await?;
    if !(200..300).contains(&response.status_code()) {
        let message = format!("feed returned HTTP {}", response.status_code());
        update_feed_status(&db, feed.id, "error", Some(message.clone())).await?;
        return Err(Error::RustError(message));
    }

    let content = response.text().await?;
    let articles = match parse_document(&content, feed.id) {
        Ok(articles) => articles,
        Err(error) => {
            update_feed_status(&db, feed.id, "error", Some(error.to_string())).await?;
            return Err(error);
        }
    };

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

    update_feed_status(&db, feed.id, "active", None).await
}

async fn update_feed_status(
    db: &worker::D1Database,
    feed_id: i32,
    status: &str,
    error_message: Option<String>,
) -> Result<()> {
    let args = [
        D1Type::Text(status),
        d1_text_or_null(&error_message),
        D1Type::Integer(feed_id),
    ];
    db.prepare(
        "UPDATE feeds SET status = ?1, error_message = ?2, last_fetched_at = CURRENT_TIMESTAMP,
         updated_at = CURRENT_TIMESTAMP WHERE id = ?3",
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
                        articles.push(Article {
                            id: 0,
                            feed_id,
                            title: parsed.title,
                            link: parsed.link,
                            guid,
                            summary: parsed.summary,
                            content: parsed.content,
                            published_at: parsed.published_at,
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
            Some("Tue, 01 Sep 2026 10:20:30 GMT")
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
            Some("Wed, 02 Sep 2026 08:00:00 GMT")
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

}


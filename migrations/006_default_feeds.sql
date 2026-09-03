-- 006: production default feed bootstrap (ONE-TIME, idempotent)
--
-- Semantics: this is a bootstrap for an EMPTY / freshly-migrated database, not a
-- permanent reconcile. It seeds the current 3 production feeds guarded by
-- canonical_url (uq_feeds_normalized_url) so re-running never duplicates. A feed
-- deliberately deleted later is NOT restored by re-running this file.
--
-- next_fetch_at = NULL relies on the scheduler's existing due rule
-- (src/scheduler.rs: enabled=1 AND (last_fetched_at IS NULL OR next_fetch_at IS
-- NULL OR next_fetch_at <= datetime('now'))), so a seeded feed is fetched on the
-- first cron tick after install.

INSERT INTO feeds (url, title, status, normalized_url, fetch_interval_minutes, enabled, next_fetch_at)
SELECT 'https://rss.nytimes.com/services/xml/rss/nyt/World.xml', 'NYT World', 'active',
       'https://rss.nytimes.com/services/xml/rss/nyt/World.xml', 15, 1, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM feeds WHERE normalized_url = 'https://rss.nytimes.com/services/xml/rss/nyt/World.xml'
);

INSERT INTO feeds (url, title, status, normalized_url, fetch_interval_minutes, enabled, next_fetch_at)
SELECT 'https://feeds.bbci.co.uk/news/rss.xml', 'BBC News', 'active',
       'https://feeds.bbci.co.uk/news/rss.xml', 15, 1, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM feeds WHERE normalized_url = 'https://feeds.bbci.co.uk/news/rss.xml'
);

INSERT INTO feeds (url, title, status, normalized_url, fetch_interval_minutes, enabled, next_fetch_at)
SELECT 'https://openai.com/news/rss.xml', 'OpenAI News', 'active',
       'https://openai.com/news/rss.xml', 15, 1, NULL
WHERE NOT EXISTS (
    SELECT 1 FROM feeds WHERE normalized_url = 'https://openai.com/news/rss.xml'
);

-- User-scoped RSS sources (incremental: keeps legacy feeds/articles intact).
CREATE TABLE IF NOT EXISTS rss_sources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL DEFAULT 'demo',
    url TEXT NOT NULL,
    title TEXT,
    site_url TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    fetch_interval_minutes INTEGER NOT NULL DEFAULT 60,
    last_fetched_at TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    error_message TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_id, url)
);

-- Articles fetched for user-scoped sources.
CREATE TABLE IF NOT EXISTS rss_articles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL,
    source_id INTEGER NOT NULL,
    title TEXT NOT NULL,
    link TEXT NOT NULL,
    guid TEXT NOT NULL,
    summary TEXT,
    content TEXT,
    published_at TEXT,
    hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(user_id, source_id, guid)
);

CREATE INDEX IF NOT EXISTS idx_rss_sources_user ON rss_sources(user_id);
CREATE INDEX IF NOT EXISTS idx_rss_sources_due ON rss_sources(enabled, last_fetched_at);
CREATE INDEX IF NOT EXISTS idx_rss_articles_source ON rss_articles(user_id, source_id);

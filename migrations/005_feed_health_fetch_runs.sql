-- 005: Feed health + fetch-run observability (production data plane hardening)
-- Run once per database. SQLite does not support "ADD COLUMN IF NOT EXISTS".

ALTER TABLE feeds ADD COLUMN normalized_url TEXT;
ALTER TABLE feeds ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
ALTER TABLE feeds ADD COLUMN fetch_interval_minutes INTEGER NOT NULL DEFAULT 15;
ALTER TABLE feeds ADD COLUMN last_success_at TEXT;
ALTER TABLE feeds ADD COLUMN last_failure_at TEXT;
ALTER TABLE feeds ADD COLUMN last_http_status INTEGER;
ALTER TABLE feeds ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE feeds ADD COLUMN next_fetch_at TEXT;
ALTER TABLE feeds ADD COLUMN etag TEXT;
ALTER TABLE feeds ADD COLUMN last_modified TEXT;

-- Backfill identity so the unique index below can be created.
UPDATE feeds SET normalized_url = url WHERE normalized_url IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS uq_feeds_normalized_url ON feeds(normalized_url);
CREATE INDEX IF NOT EXISTS idx_feeds_due ON feeds(enabled, next_fetch_at);

-- One row per scheduler cycle: Cron -> Scheduler -> Queue -> Consumer -> Persist.
CREATE TABLE IF NOT EXISTS fetch_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at TEXT,
    trigger TEXT NOT NULL,
    run_key TEXT NOT NULL,
    feeds_scheduled INTEGER NOT NULL DEFAULT 0,
    feeds_fetched INTEGER NOT NULL DEFAULT 0,
    feeds_failed INTEGER NOT NULL DEFAULT 0,
    articles_inserted INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running',
    error TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_fetch_runs_key ON fetch_runs(run_key);
CREATE INDEX IF NOT EXISTS idx_fetch_runs_started ON fetch_runs(started_at);

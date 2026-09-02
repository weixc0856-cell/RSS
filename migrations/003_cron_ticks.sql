-- Diagnostic heartbeat for cron/scheduled invocations (see scheduler.rs).
CREATE TABLE IF NOT EXISTS cron_ticks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cron TEXT NOT NULL,
    fired_at TEXT NOT NULL DEFAULT (datetime('now'))
);

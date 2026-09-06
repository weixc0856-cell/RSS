-- 007: anonymous device namespace registry (per-device subscription isolation)
--
-- X-User-Id (a random UUID minted by each browser) → INTEGER id.
-- subscriptions.user_id now references profiles.id. This table is NOT a user
-- or account table and carries NO authentication/authorization semantics — it
-- merely maps one anonymous device key to the subscription rows it owns, so
-- callers with different keys never see each other's lists (namespacing, not a
-- security boundary). Device keys are random UUIDs: collision between devices
-- is practically impossible, never unforgeable — anyone may mint any key.
--
-- The existing subscriptions table (001) is unchanged; prod has 0 rows, so no
-- backfill is needed. profile rows are never deleted (no device-lifecycle
-- requirement today).

CREATE TABLE IF NOT EXISTS profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    device_key TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

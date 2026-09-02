use serde_json::Value;
use worker::*;

/// Cron entry point (see `[triggers] crons` in `wrangler.toml.template`).
///
/// Design (C-plan): cron only *schedules*; it selects feeds that are due for a
/// refresh and pushes a small `{ feed_id, url }` job onto `RSS_FETCH_QUEUE`.
/// The actual fetching/parsing/persisting runs inside the queue consumer
/// (`queue::consume`), so one slow feed cannot stall the whole round and
/// failures can be retried independently.
#[event(scheduled)]
pub async fn run(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) -> Result<()> {
    // Unconditional first-line marker so we can tell "cron fired" apart from
    // "cron fired but business logic produced no DB change".
    let cron = event.cron();
    console_log!("[scheduler] CRON FIRED cron={}", cron);

    let db = crate::db::get_db(&env)?;
    // Heartbeat row: unambiguous proof that Cloudflare invoked this handler.
    db.prepare("INSERT INTO cron_ticks (cron) VALUES (?1)")
        .bind(&[cron.into()])?
        .run()
        .await?;
    let stmt = db.prepare(
        "SELECT id, url FROM feeds
         WHERE last_fetched_at IS NULL
            OR last_fetched_at <= datetime('now', '-1 hour')
         ORDER BY id",
    );
    let rows = stmt.all().await?;
    let feeds = rows.results::<Value>()?;

    let queue = env.queue("RSS_FETCH_QUEUE")?;
    let mut sent = 0usize;
    for row in feeds {
        let feed_id = row["id"].as_i64().unwrap_or(0);
        let url = row["url"].as_str().unwrap_or("");
        if feed_id <= 0 || url.is_empty() {
            continue;
        }
        queue
            .send(serde_json::json!({ "feed_id": feed_id, "url": url }))
            .await?;
        sent += 1;
    }

    // User-scoped sources: honour per-source enabled flag and interval.
    let src_stmt = db.prepare(
        "SELECT id, user_id, url FROM rss_sources
         WHERE enabled = 1
           AND (last_fetched_at IS NULL
                OR (julianday('now') - julianday(last_fetched_at)) * 1440 >= fetch_interval_minutes)
         ORDER BY id",
    );
    let src_rows = src_stmt.all().await?;
    let sources = src_rows.results::<Value>()?;
    let mut sent_sources = 0usize;
    for row in sources {
        let source_id = row["id"].as_i64().unwrap_or(0);
        let user_id = row["user_id"].as_str().unwrap_or("");
        let url = row["url"].as_str().unwrap_or("");
        if source_id <= 0 || user_id.is_empty() || url.is_empty() {
            continue;
        }
        queue
            .send(serde_json::json!({
                "source_id": source_id,
                "user_id": user_id,
                "url": url
            }))
            .await?;
        sent_sources += 1;
    }

    console_log!(
        "[scheduler] queued {} legacy feed(s) and {} user source(s)",
        sent,
        sent_sources
    );
    Ok(())
}

use serde_json::Value;
use worker::*;

use crate::utils::sqlite_now;

/// Cron entry point (see `[triggers] crons` in `wrangler.toml.template`).
///
/// Design:
///  - Cron only *wakes up* the scheduler.
///  - The scheduler selects feeds that are DUE (`enabled = 1` and
///    `next_fetch_at <= now()`, or never fetched yet) and pushes a small JSON
///    job onto `RSS_FETCH_QUEUE`. Fetching/parsing/persisting happens in the
///    queue consumer so one slow feed cannot stall the round.
///  - Every cron cycle is recorded in `fetch_runs` keyed by the current minute
///    (`run_key`), making the scheduler idempotent: re-fires inside the same
///    minute do not double-enqueue.
#[event(scheduled)]
pub async fn run(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) -> Result<()> {
    let cron = event.cron();
    console_log!("[scheduler] CRON FIRED cron={}", cron);

    let db = crate::db::get_db(&env)?;
    // Heartbeat row: unambiguous proof that Cloudflare invoked this handler.
    db.prepare("INSERT INTO cron_ticks (cron) VALUES (?1)")
        .bind(&[cron.clone().into()])?
        .run()
        .await?;

    // ---- create / reuse the fetch_run for this minute (idempotency) ---------
    let run_key = sqlite_now()[..16].to_string(); // YYYY-MM-DD HH:MM

    // Close any previous run left in 'running' (crash/timeout safety).
    db.prepare(
        "UPDATE fetch_runs SET status = 'partial', error = 'superseded',
           finished_at = COALESCE(finished_at, datetime('now'))
         WHERE status = 'running' AND run_key < ?1",
    )
    .bind_refs(vec![worker::d1::D1Type::Text(run_key.as_str())].iter())?
    .run()
    .await?;

    let existing = db
        .prepare("SELECT id FROM fetch_runs WHERE run_key = ?1")
        .bind_refs(
            vec![worker::d1::D1Type::Text(run_key.as_str())].iter(),
        )?
        .first::<Value>(None)
        .await?;
    let run_id: i64 = if let Some(row) = existing {
        // Already scheduled this minute — skip to avoid double enqueue.
        console_log!("[scheduler] run_key {} already processed, skipping", run_key);
        return Ok(());
    } else {
        let trigger = format!("cron:{cron}");
        db.prepare(
            "INSERT INTO fetch_runs (started_at, trigger, run_key, status) VALUES (datetime('now'), ?1, ?2, 'running')",
        )
        .bind_refs(
            vec![
                worker::d1::D1Type::Text(trigger.as_str()),
                worker::d1::D1Type::Text(run_key.as_str()),
            ]
            .iter(),
        )?
        .run()
        .await?;
        db.prepare("SELECT id FROM fetch_runs WHERE run_key = ?1")
            .bind_refs(vec![worker::d1::D1Type::Text(run_key.as_str())].iter())?
            .first::<Value>(None)
            .await?
            .and_then(|row| row["id"].as_i64())
            .ok_or_else(|| Error::RustError("fetch_run id missing".to_string()))?
    };

    // ---- select due feeds (next_fetch_at based) -----------------------------
    // Subscription gate: a feed is only scheduled when at least one device is
    // subscribed. 0-subscription pool feeds are dormant — Discover-visible but
    // not fetched (see ARCHITECTURE.md pool state machine). The consumer-side
    // execution gate in fetch_feed re-checks this right before persisting.
    let stmt = db.prepare(
        "SELECT f.id, f.url FROM feeds f
         WHERE f.enabled = 1
           AND EXISTS (SELECT 1 FROM subscriptions s WHERE s.feed_id = f.id)
           AND (f.last_fetched_at IS NULL
                OR f.next_fetch_at IS NULL
                OR f.next_fetch_at <= datetime('now'))
         ORDER BY f.id",
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
        // NOTE: send a JSON *string* (not a serde_json::Value object). worker-rs
        // serializes `Value` through serde-wasm-bindgen into a JS object whose
        // properties are dropped by the runtime's queue.send -> "{}" on arrival.
        // Sending a string with contentType "json" keeps the fields intact.
        let payload = serde_json::to_string(&serde_json::json!({
            "version": 1,
            "type": "feed_fetch",
            "feed_id": feed_id,
            "url": url,
            "run_id": run_id
        }))
        .map_err(|e| Error::RustError(e.to_string()))?;
        queue.send(payload.as_str()).await?;
        sent += 1;
    }

    // ---- finalize scheduling side of the run ----------------------------------
    let total = sent;
    // Literal numeric SQL: values are integers we produced; avoids binding issues
    // in this hot path and keeps the run row accurate even if a consumer is slow.
    let finalize_sql = format!(
        "UPDATE fetch_runs SET feeds_scheduled = {total},
           finished_at = CASE WHEN {total} = 0 THEN datetime('now') ELSE finished_at END,
           status = CASE WHEN {total} = 0 THEN 'ok' ELSE status END
         WHERE id = {run_id}"
    );
    let _ = db.prepare(&finalize_sql).run().await;

    // If nothing was scheduled this run, pre-seed the next window so freshly
    // migrated feeds with NULL next_fetch_at get a sane cadence going forward.
    if total == 0 {
        let backfill = db.prepare(
            "UPDATE feeds SET next_fetch_at = datetime('now', '+' || fetch_interval_minutes || ' minutes')
             WHERE enabled = 1 AND next_fetch_at IS NULL",
        );
        backfill.run().await?;
    }

    console_log!("[scheduler] run_id={} queued {} feed(s)", run_id, sent);
    Ok(())
}

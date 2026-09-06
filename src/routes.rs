use worker::{Request, Response, Result, Env};
use serde_json::Value;
use crate::types::*;
use crate::db;

/// Feed columns shared by the global pool catalog (`list_feeds`) and the
/// per-device list (`handle_get_my_feeds`). The frontend derives each nav
/// feed's health from exactly these fields, so BOTH endpoints MUST return the
/// same projection — drift silently degrades the health badges. `/api/feeds`
/// stays the shared-pool catalog (Discover-only for the frontend);
/// `/api/me/feeds` is the only source the nav may read.
const FEED_PROJECTION: &str = "id, url, title, site_url, favicon_url, last_fetched_at, status, \
     error_message, enabled, fetch_interval_minutes, \
     last_success_at, last_failure_at, last_http_status, \
     consecutive_failures, next_fetch_at, \
     normalized_url, created_at, updated_at";

pub async fn health() -> Result<Response> {
    Response::ok("ok")
}

pub async fn list_feeds(env: Env) -> Result<Response> {
    let db = db::get_db(&env)?;
    let stmt = db.prepare(&format!("SELECT {FEED_PROJECTION} FROM feeds ORDER BY id DESC"));
    let rows = stmt.all().await?;
    let feeds = rows.results::<Value>()?;

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(feeds),
        error: None,
    })
}

/// Resolve the caller's device profile id. The error carries a 400-worthy
/// message ("X-User-Id header required"); handlers map it to a structured
/// `json_error` body. `X-User-Id` is a device *namespace* identifier, not
/// authentication (see crate::identity).
async fn require_device(req: &Request, env: &Env) -> std::result::Result<i32, String> {
    crate::identity::require_profile(req, env)
        .await
        .map_err(|_| "X-User-Id header required".to_string())
}

/// One feed row in the canonical projection (used in create/subscribe responses
/// so the returned feed is always the same shape `list_feeds` / the nav expect).
async fn select_feed_row(db: &worker::D1Database, feed_id: i32) -> Result<Option<Value>> {
    let stmt = db.prepare(&format!("SELECT {FEED_PROJECTION} FROM feeds WHERE id = ?1"));
    Ok(stmt.bind(&[feed_id.into()])?.first::<Value>(None).await?)
}

/// Subscribe a device to a feed, idempotently. `UNIQUE(user_id, feed_id)` makes
/// a repeated subscribe a no-op.
async fn insert_subscription(db: &worker::D1Database, profile_id: i32, feed_id: i32) -> Result<()> {
    db.prepare("INSERT OR IGNORE INTO subscriptions (user_id, feed_id) VALUES (?1, ?2)")
        .bind(&[profile_id.into(), feed_id.into()])?
        .run()
        .await?;
    Ok(())
}

/// Is this device already subscribed to the feed?
async fn is_subscribed(db: &worker::D1Database, profile_id: i32, feed_id: i32) -> Result<bool> {
    let row = db
        .prepare("SELECT 1 FROM subscriptions WHERE user_id = ?1 AND feed_id = ?2")
        .bind(&[profile_id.into(), feed_id.into()])?
        .first::<Value>(None)
        .await?;
    Ok(row.is_some())
}

/// Find-or-create a shared-pool feed for this URL and subscribe THIS device to
/// it. Fully idempotent and always `success:true` for a valid body:
///   - pool has no such feed        -> create feed + subscribe + enqueue first fetch
///   - pool has it, I am not subbed  -> subscribe silently
///   - pool has it, I am subbed      -> no-op
/// data = { feed: <full projection row>, created: bool, already: bool } —
/// `created` = brand-new in the shared pool, `already` = this device was already
/// subscribed before this call (drives honest frontend copy).
pub async fn add_feed(mut req: Request, env: Env) -> Result<Response> {
    // Device identity FIRST — the header must be read before `req.json()` takes
    // the body.
    let profile_id = match require_device(&req, &env).await {
        Ok(id) => id,
        Err(message) => return json_error(&message, 400),
    };

    let payload = match req.json::<serde_json::Value>().await {
        Ok(p) => p,
        Err(e) => {
            return Response::from_json(&ApiResponse::<()> {
                success: false,
                data: None,
                error: Some(format!("Invalid JSON: {}", e)),
            })
        }
    };

    let url = payload["url"].as_str().unwrap_or("").trim();
    if url.is_empty() {
        return Response::from_json(&ApiResponse::<()> {
            success: false,
            data: None,
            error: Some("url is required".to_string()),
        });
    }

    let title = payload["title"].as_str().unwrap_or("RSS Feed");
    let canonical = crate::utils::canonical_url(url);

    let db = db::get_db(&env)?;

    // Registry identity on the shared pool is the canonical URL (unique index backs it).
    let existing = db
        .prepare("SELECT id FROM feeds WHERE normalized_url = ?1")
        .bind(&[canonical.clone().into()])?
        .first::<Value>(None)
        .await?;

    let (feed_row, created, already) = if let Some(row) = existing {
        let feed_id = row["id"]
            .as_i64()
            .ok_or_else(|| worker::Error::RustError("existing feed row missing id".to_string()))?
            as i32;
        let already = is_subscribed(&db, profile_id, feed_id).await?;
        if !already {
            insert_subscription(&db, profile_id, feed_id).await?;
        }
        let feed_row = select_feed_row(&db, feed_id)
            .await?
            .ok_or_else(|| worker::Error::RustError("subscribed feed row missing".to_string()))?;
        (feed_row, false, already)
    } else {
        // Brand-new pool feed. Subscribe THIS device BEFORE the initial-fetch
        // enqueue so the consumer's execution gate (feed must still have >=1
        // subscriber) passes when the queued job runs.
        let interval = payload["fetch_interval_minutes"]
            .as_i64()
            .unwrap_or(15)
            .clamp(5, 1440);
        let stmt = db.prepare(
            "INSERT INTO feeds (url, title, status, normalized_url, fetch_interval_minutes, next_fetch_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) RETURNING id",
        );
        let args = vec![
            worker::d1::D1Type::Text(url),
            worker::d1::D1Type::Text(title),
            worker::d1::D1Type::Text("pending"),
            worker::d1::D1Type::Text(canonical.as_str()),
            worker::d1::D1Type::Integer(interval as i32),
        ];
        let created_row = stmt
            .bind_refs(args.iter())?
            .first::<Value>(None)
            .await?
            .ok_or_else(|| worker::Error::RustError("feed insert returned no row".to_string()))?;
        let feed_id = created_row["id"]
            .as_i64()
            .ok_or_else(|| worker::Error::RustError("feed insert missing id".to_string()))? as i32;
        insert_subscription(&db, profile_id, feed_id).await?;

        // Best-effort initial fetch (mirrors pre-WS7 behavior): enqueue so the
        // first articles arrive without waiting for the cron pass. Never fails
        // the create — on any error the scheduler remains the fallback.
        crate::queue::enqueue_initial_fetch(
            &env,
            serde_json::json!({
                "version": 1,
                "type": "feed_fetch",
                "feed_id": feed_id,
                "url": url,
            }),
            "feed",
        )
        .await;

        let feed_row = select_feed_row(&db, feed_id)
            .await?
            .ok_or_else(|| worker::Error::RustError("created feed row missing".to_string()))?;
        (feed_row, true, false)
    };

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(serde_json::json!({
            "feed": feed_row,
            "created": created,
            "already": already,
        })),
        error: None,
    })
}

pub async fn handle_get_feeds(env: Env) -> Result<Response> {
    list_feeds(env).await
}

pub async fn handle_create_feed(req: Request, env: Env) -> Result<Response> {
    add_feed(req, env).await
}

/// THIS device's feed list (navigation-only). The frontend nav MUST derive from
/// this endpoint, never from `GET /api/feeds` (which is the shared-pool
/// catalog, Discover-only). Same projection as `list_feeds` so per-feed health
/// renders identically, plus `subscribed_at` and the feed's article count.
pub async fn handle_get_my_feeds(req: Request, env: Env) -> Result<Response> {
    let profile_id = match require_device(&req, &env).await {
        Ok(id) => id,
        Err(message) => return json_error(&message, 400),
    };
    let db = db::get_db(&env)?;
    let stmt = db.prepare(&format!(
        "SELECT {FEED_PROJECTION},
                s.subscribed_at,
                (SELECT COUNT(*) FROM articles a WHERE a.feed_id = f.id) AS article_count
         FROM feeds f JOIN subscriptions s ON s.feed_id = f.id
         WHERE s.user_id = ?1 ORDER BY f.id DESC"
    ));
    let rows = stmt.bind(&[profile_id.into()])?.all().await?;
    let feeds = rows.results::<Value>()?;
    Response::from_json(&ApiResponse {
        success: true,
        data: Some(feeds),
        error: None,
    })
}

/// Subscribe THIS device to an existing shared-pool feed (Discover "+"),
/// idempotent. 404 when the feed id is no longer in the pool (it may have been
/// pruned since the Discover strip rendered).
pub async fn handle_subscribe_device(feed_id: i32, req: Request, env: Env) -> Result<Response> {
    let profile_id = match require_device(&req, &env).await {
        Ok(id) => id,
        Err(message) => return json_error(&message, 400),
    };
    let db = db::get_db(&env)?;

    let feed_row = match select_feed_row(&db, feed_id).await? {
        Some(row) => row,
        None => return json_error("Feed not found", 404),
    };
    insert_subscription(&db, profile_id, feed_id).await?;

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(feed_row),
        error: None,
    })
}

pub async fn handle_get_articles(feed_id: i32, env: Env) -> Result<Response> {
    let db = db::get_db(&env)?;
    let stmt = db
        .prepare(
            "SELECT id, feed_id, title, link, guid, summary, content, published_at, hash
             FROM articles WHERE feed_id = ?1
             ORDER BY published_at DESC LIMIT 50",
        )
        .bind(&[feed_id.into()])?;
    let rows = stmt.all().await?;
    let articles = rows.results::<Article>()?;
    Response::from_json(&ApiResponse {
        success: true,
        data: Some(articles),
        error: None,
    })
}

/// Fetch a feed from its origin, parse it and persist new articles (D1),
/// then update the feed status to `active`/`error`.
pub async fn handle_fetch_feed(feed_id: i32, env: Env) -> Result<Response> {
    let db = db::get_db(&env)?;
    let feed = db
        .prepare(
            "SELECT id, url, title, site_url, favicon_url, last_fetched_at, status
             FROM feeds WHERE id = ?1",
        )
        .bind(&[feed_id.into()])?
        .first::<Feed>(None)
        .await?;

    let feed = match feed {
        Some(feed) => feed,
        None => return Response::error("Feed not found", 404),
    };

    if let Err(error) = crate::feed::fetch_feed(&feed.url, &env).await {
        return Response::from_json(&ApiResponse::<()> {
            success: false,
            data: None,
            error: Some(format!("Failed to fetch feed: {}", error)),
        });
    }

    // Report how many articles are now persisted for this feed.
    let row = db
        .prepare("SELECT COUNT(*) AS total FROM articles WHERE feed_id = ?1")
        .bind(&[feed_id.into()])?
        .first::<Value>(None)
        .await?;
    let total = row.unwrap_or(serde_json::json!({ "total": 0 }));
    Response::from_json(&ApiResponse {
        success: true,
        data: Some(total),
        error: None,
    })
}

/// Read-only production diagnostics: feed status distribution, article count,
/// failed feeds (with error_message), cron heartbeat summary and fetch-run
/// lifecycle health.
pub async fn handle_diagnostics(env: Env) -> Result<Response> {
    let db = db::get_db(&env)?;

    let by_status = db
        .prepare(
            "SELECT status, COUNT(*) AS c FROM feeds
             WHERE enabled = 1 GROUP BY status ORDER BY status",
        )
        .all()
        .await?
        .results::<Value>()?;

    let articles_total = db
        .prepare("SELECT COUNT(*) AS total FROM articles")
        .all()
        .await?
        .results::<Value>()?;

    let failed = db
        .prepare(
            "SELECT id, title, url, error_message, last_fetched_at, last_failure_at,
                    last_http_status, consecutive_failures, next_fetch_at
             FROM feeds WHERE status = 'error' AND enabled = 1
             ORDER BY id LIMIT 20",
        )
        .all()
        .await?
        .results::<Value>()?;

    let cron = db
        .prepare("SELECT COUNT(*) AS ticks, MAX(fired_at) AS last_tick FROM cron_ticks")
        .all()
        .await?
        .results::<Value>()?;

    let last_run = db
        .prepare(
            "SELECT id, started_at, finished_at, trigger, run_key,
                    feeds_scheduled, feeds_fetched, feeds_failed, articles_inserted, status
             FROM fetch_runs ORDER BY id DESC LIMIT 1",
        )
        .all()
        .await?
        .results::<Value>()?;

    // Dormant prototype layer (rss_sources / rss_articles): surfaced here so
    // diagnostics is model-complete. Pure additive — the feeds/articles fields
    // above are untouched.
    let sources_by_status = db
        .prepare(
            "SELECT status, COUNT(*) AS c FROM rss_sources
             WHERE enabled = 1 GROUP BY status ORDER BY status",
        )
        .all()
        .await?
        .results::<Value>()?;

    let sources_total = db
        .prepare(
            "SELECT (SELECT COUNT(*) FROM rss_sources) AS sources,
                    (SELECT COUNT(*) FROM rss_articles) AS articles",
        )
        .all()
        .await?
        .results::<Value>()?
        .first()
        .cloned()
        .unwrap_or_default();

    let data = serde_json::json!({
        "feeds_by_status": by_status,
        "articles_total": articles_total,
        "failed_feeds": failed,
        "cron_ticks": cron,
        "last_fetch_run": last_run.first(),
        "rss_sources": {
            "total": sources_total["sources"].as_i64().unwrap_or(0),
            "by_status": sources_by_status,
        },
        // Intentionally mirrors the legacy `articles_total` array shape
        // ([{ "total": N }]) for additive compatibility — no shape
        // normalization is attempted in this round.
        "rss_articles_total": serde_json::json!([
            { "total": sources_total["articles"].as_i64().unwrap_or(0) }
        ]),
        "generated_at": crate::utils::current_timestamp(),
    });

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(data),
        error: None,
    })
}

/// Production health / freshness endpoint so the frontend can distinguish
/// "the news itself is old" from "the RSS pipeline is stale".
pub async fn handle_health(env: Env) -> Result<Response> {
    let db = db::get_db(&env)?;
    let environment = env
        .var("ENVIRONMENT")
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let counts = db
        .prepare(
            "SELECT
                COUNT(*) AS total,
                SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END) AS active,
                SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS failed
             FROM feeds WHERE enabled = 1",
        )
        .all()
        .await?
        .results::<Value>()?;
    let c = counts.first().cloned().unwrap_or_default();

    let articles = db
        .prepare(
            "SELECT COUNT(*) AS total,
                    MAX(published_at) AS newest_published,
                    MAX(created_at) AS newest_stored
             FROM articles",
        )
        .all()
        .await?
        .results::<Value>()?;
    let a = articles.first().cloned().unwrap_or_default();

    let last_run = db
        .prepare(
            "SELECT id, started_at, finished_at, feeds_scheduled, feeds_fetched,
                    feeds_failed, articles_inserted, status
             FROM fetch_runs ORDER BY id DESC LIMIT 1",
        )
        .all()
        .await?
        .results::<Value>()?
        .into_iter()
        .next()
        .unwrap_or(serde_json::json!({}));

    let oldest_success = db
        .prepare(
            "SELECT MIN(last_success_at) AS oldest FROM feeds
             WHERE enabled = 1 AND status = 'active'",
        )
        .all()
        .await?
        .results::<Value>()?
        .first()
        .cloned()
        .unwrap_or_default();

    let data = serde_json::json!({
        "environment": environment,
        "generated_at": crate::utils::current_timestamp(),
        "feeds": {
            "total": c["total"].as_i64().unwrap_or(0),
            "active": c["active"].as_i64().unwrap_or(0),
            "failed": c["failed"].as_i64().unwrap_or(0),
        },
        "articles": {
            "total": a["total"].as_i64().unwrap_or(0),
            "newest_published_at": a["newest_published"],
            "newest_stored_at": a["newest_stored"],
        },
        "scheduler": {
            "last_run": last_run,
            "oldest_successful_feed_at": oldest_success["oldest"],
        },
    });

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(data),
        error: None,
    })
}

/// JSON error body for business APIs that already speak the `ApiResponse`
/// contract — only for turning prior `success:true` + error-text lies into an
/// honest status. Plain HTTP errors (400/404/405…) keep `Response::error`.
fn json_error(message: &str, status: u16) -> Result<Response> {
    let response = Response::from_json(&ApiResponse::<()> {
        success: false,
        data: None,
        error: Some(message.to_string()),
    })?;
    Ok(response.with_status(status))
}

/// THIS device unsubscribes from a shared-pool feed. When it was the last
/// subscriber the pool entry is pruned too (feed + its articles) so an unowned
/// feed is never left fetching. Explicit delete order (articles before feed)
/// keeps the contract visible and independent of any implicit FK cascade.
/// Returns `{ id, pruned }` so the caller can report whether the pool was hit.
pub async fn handle_unsubscribe(feed_id: i32, req: Request, env: Env) -> Result<Response> {
    let profile_id = match require_device(&req, &env).await {
        Ok(id) => id,
        Err(message) => return json_error(&message, 400),
    };
    let db = db::get_db(&env)?;

    // Feed must exist. A 404 also does not leak whether OTHER devices subscribe.
    let existing = db
        .prepare("SELECT id FROM feeds WHERE id = ?1")
        .bind(&[feed_id.into()])?
        .first::<Value>(None)
        .await?;
    if existing.is_none() {
        return json_error("Feed not found", 404);
    }

    // This device must be subscribed — refusing when it is not keeps pool
    // membership of other devices private.
    if !is_subscribed(&db, profile_id, feed_id).await? {
        return json_error("Not subscribed to this feed", 404);
    }

    // Remove this device's subscription, then decide whether the pool entry is
    // now unowned (last subscriber → prune).
    db.prepare("DELETE FROM subscriptions WHERE user_id = ?1 AND feed_id = ?2")
        .bind(&[profile_id.into(), feed_id.into()])?
        .run()
        .await?;

    let remaining = db
        .prepare("SELECT COUNT(*) AS c FROM subscriptions WHERE feed_id = ?1")
        .bind(&[feed_id.into()])?
        .first::<Value>(None)
        .await?;
    let left = remaining.and_then(|r| r["c"].as_i64()).unwrap_or(0);
    let mut pruned = false;
    if left == 0 {
        db.prepare("DELETE FROM articles WHERE feed_id = ?1")
            .bind(&[feed_id.into()])?
            .run()
            .await?;
        db.prepare("DELETE FROM feeds WHERE id = ?1")
            .bind(&[feed_id.into()])?
            .run()
            .await?;
        pruned = true;
    }

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(serde_json::json!({ "id": feed_id, "pruned": pruned })),
        error: None,
    })
}

/// Retired API — the user-scoped `/api/sources` prototype layer is dormant (see
/// ARCHITECTURE.md §3): feeds/articles is the production model. Every request
/// here (any method, any sub-path) answers an honest 501 and is guaranteed to
/// never touch D1 — a retired API, not a half-usable one.
pub async fn handle_sources_retired() -> Result<Response> {
    json_error(
        "source API is retired (rss_sources is a dormant prototype layer)",
        501,
    )
}

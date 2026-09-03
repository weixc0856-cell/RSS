use worker::{Request, Response, Result, Env};
use serde_json::Value;
use crate::types::*;
use crate::db;

pub async fn health() -> Result<Response> {
    Response::ok("ok")
}

pub async fn list_feeds(env: Env) -> Result<Response> {
    let db = db::get_db(&env)?;
    let stmt = db.prepare(
        "SELECT id, url, title, site_url, favicon_url, last_fetched_at, status,
                error_message, enabled, fetch_interval_minutes,
                last_success_at, last_failure_at, last_http_status,
                consecutive_failures, next_fetch_at,
                normalized_url, created_at, updated_at
         FROM feeds ORDER BY id DESC",
    );
    let rows = stmt.all().await?;
    let feeds = rows.results::<Value>()?;

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(feeds),
        error: None,
    })
}

pub async fn add_feed(mut req: Request, env: Env) -> Result<Response> {
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

    // Registry identity: reject duplicates by canonical URL (unique index backs this).
    let existing = db
        .prepare("SELECT id FROM feeds WHERE normalized_url = ?1")
        .bind(&[canonical.clone().into()])?
        .first::<Value>(None)
        .await?;
    if existing.is_some() {
        return Response::from_json(&ApiResponse::<()> {
            success: false,
            data: None,
            error: Some("feed already exists".to_string()),
        });
    }

    // New feed is due immediately (last_fetched_at IS NULL already covers this,
    // but an explicit next_fetch_at keeps the scheduler query self-describing).
    let interval = payload["fetch_interval_minutes"]
        .as_i64()
        .unwrap_or(15)
        .clamp(5, 1440);
    let stmt = db.prepare(
        "INSERT INTO feeds (url, title, status, normalized_url, fetch_interval_minutes, next_fetch_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) RETURNING *",
    );

    let args = vec![
        worker::d1::D1Type::Text(url),
        worker::d1::D1Type::Text(title),
        worker::d1::D1Type::Text("pending"),
        worker::d1::D1Type::Text(canonical.as_str()),
        worker::d1::D1Type::Integer(interval as i32),
    ];

    match stmt.bind_refs(args.iter()) {
        Ok(bound_stmt) => match bound_stmt.first::<Value>(None).await {
            Ok(result) => Response::from_json(&ApiResponse {
                success: true,
                data: result,
                error: None,
            }),
            Err(e) => Response::from_json(&ApiResponse::<()> {
                success: false,
                data: None,
                error: Some(format!("Failed to insert feed: {}", e)),
            }),
        },
        Err(e) => Response::from_json(&ApiResponse::<()> {
            success: false,
            data: None,
            error: Some(format!("Failed to prepare statement: {}", e)),
        }),
    }
}

pub async fn handle_get_feeds(env: Env) -> Result<Response> {
    list_feeds(env).await
}

pub async fn handle_create_feed(req: Request, env: Env) -> Result<Response> {
    add_feed(req, env).await
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

    let data = serde_json::json!({
        "feeds_by_status": by_status,
        "articles_total": articles_total,
        "failed_feeds": failed,
        "cron_ticks": cron,
        "last_fetch_run": last_run.first(),
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

pub async fn handle_get_user_feeds(user_id: i32) -> Result<Response> {
    Response::from_json(&ApiResponse::<Vec<Feed>> {
        success: true,
        data: Some(Vec::new()),
        error: None,
    })
}

pub async fn handle_subscribe_feed(mut req: Request) -> Result<Response> {
    match req.json::<SubscribeFeedRequest>().await {
        Ok(_payload) => {
            Response::from_json(&ApiResponse::<Subscription> {
                success: true,
                data: None,
                error: Some("Not implemented".to_string()),
            })
        }
        Err(e) => {
            Response::from_json(&ApiResponse::<()> {
                success: false,
                data: None,
                error: Some(format!("Invalid request: {}", e)),
            })
        }
    }
}

pub async fn handle_delete_feed(feed_id: i32) -> Result<Response> {
    Response::from_json(&ApiResponse::<()> {
        success: true,
        data: None,
        error: Some("Not implemented".to_string()),
    })
}

pub async fn handle_unsubscribe_feed(user_id: i32, feed_id: i32) -> Result<Response> {
    Response::from_json(&ApiResponse::<()> {
        success: true,
        data: None,
        error: Some("Not implemented".to_string()),
    })
}

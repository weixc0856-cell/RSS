use worker::{Request, Response, Result, Env};
use serde_json::Value;
use crate::types::*;
use crate::db;

pub async fn health() -> Result<Response> {
    Response::ok("ok")
}

pub async fn list_feeds(env: Env) -> Result<Response> {
    let db = db::get_db(&env)?;
    let stmt = db.prepare("SELECT id, url, title, site_url, favicon_url, last_fetched_at, status FROM feeds ORDER BY id DESC");
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

    let db = db::get_db(&env)?;
    
    // Use parameterized query to prevent SQL injection
    let stmt = db.prepare(
        "INSERT INTO feeds (url, title, status) VALUES (?1, ?2, ?3) RETURNING *"
    );
    
    match stmt.bind(&[url.into(), title.into(), "pending".into()]) {
        Ok(bound_stmt) => {
            match bound_stmt.first::<Value>(None).await {
                Ok(result) => {
                    Response::from_json(&ApiResponse {
                        success: true,
                        data: result,
                        error: None,
                    })
                }
                Err(e) => {
                    Response::from_json(&ApiResponse::<()> {
                        success: false,
                        data: None,
                        error: Some(format!("Failed to insert feed: {}", e)),
                    })
                }
            }
        }
        Err(e) => {
            Response::from_json(&ApiResponse::<()> {
                success: false,
                data: None,
                error: Some(format!("Failed to prepare statement: {}", e)),
            })
        }
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

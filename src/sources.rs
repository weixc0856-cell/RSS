use url::Url;
use worker::d1::D1Type;
use worker::{Env, Request, Response, Result};

use crate::db;
use crate::types::{ApiResponse, CreateSourceRequest, RssArticle, SourceItem, UpdateSourceRequest};

const SOURCE_COLS: &str = "id, user_id, url, title, site_url, enabled, fetch_interval_minutes,
    last_fetched_at, status, error_message, created_at, updated_at";

/// Cap inserts per fetch invocation (same budget as the legacy pipeline).
const MAX_ARTICLES_PER_FETCH: usize = 25;

fn is_http_url(value: &str) -> bool {
    Url::parse(value)
        .map(|u| matches!(u.scheme(), "http" | "https"))
        .unwrap_or(false)
}

fn clamp_int(value: Option<i64>, default: i32) -> i32 {
    value.map(|v| v.clamp(1, 100_000) as i32).unwrap_or(default)
}

pub async fn list_sources(user_id: &str, env: &Env) -> Result<Response> {
    let db = db::get_db(env)?;
    let stmt = db.prepare(&format!(
        "SELECT {SOURCE_COLS} FROM rss_sources WHERE user_id = ?1 ORDER BY id DESC"
    ));
    let rows = stmt.bind(&[user_id.into()])?.all().await?;
    let sources = rows.results::<SourceItem>()?;

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(sources),
        error: None,
    })
}

pub async fn create_source(mut req: Request, user_id: &str, env: &Env) -> Result<Response> {
    match create_source_inner(&mut req, user_id, env).await {
        Ok(response) => Ok(response),
        Err(error) => Ok(Response::from_json(&ApiResponse::<()> {
            success: false,
            data: None,
            error: Some(format!("create source error: {}", error)),
        })?),
    }
}

async fn create_source_inner(
    req: &mut Request,
    user_id: &str,
    env: &Env,
) -> Result<Response> {
    let payload = req.json::<CreateSourceRequest>().await?;
    let url = payload.url.trim().to_string();
    if url.is_empty() {
        return Response::error("url is required", 400);
    }
    if !is_http_url(&url) {
        return Response::error("url must be http(s)", 400);
    }

    let title = payload
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let site_url = payload
        .site_url
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let enabled = clamp_int(payload.enabled, 1);
    let interval = clamp_int(payload.fetch_interval_minutes, 60);

    let db = db::get_db(env)?;

    // Reject duplicates up-front (INSERT OR IGNORE alone would mask them).
    let existing = db
        .prepare("SELECT id FROM rss_sources WHERE user_id = ?1 AND url = ?2")
        .bind(&[user_id.into(), url.clone().into()])?
        .first::<serde_json::Value>(None)
        .await?;
    if existing.is_some() {
        return Response::error("source already exists for this user", 409);
    }

    let args: Vec<D1Type<'_>> = vec![
        D1Type::Text(user_id),
        D1Type::Text(&url),
        match &title {
            Some(value) => D1Type::Text(value),
            None => D1Type::Null,
        },
        match &site_url {
            Some(value) => D1Type::Text(value),
            None => D1Type::Null,
        },
        D1Type::Integer(enabled),
        D1Type::Integer(interval),
        D1Type::Text("active"),
    ];

    db.prepare(
        "INSERT OR IGNORE INTO rss_sources
         (user_id, url, title, site_url, enabled, fetch_interval_minutes, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind_refs(args.iter())?
    .run()
    .await?;

    let created = db
        .prepare(&format!(
            "SELECT {SOURCE_COLS} FROM rss_sources WHERE user_id = ?1 AND url = ?2"
        ))
        .bind(&[user_id.into(), url.into()])?
        .first::<SourceItem>(None)
        .await?;

    match created {
        Some(source) => Response::from_json(&ApiResponse {
            success: true,
            data: Some(source),
            error: None,
        }),
        None => Response::error("source already exists for this user", 409),
    }
}

fn d1_opt<'a>(value: &'a Option<String>) -> D1Type<'a> {
    match value {
        Some(text) => D1Type::Text(text),
        None => D1Type::Null,
    }
}

fn parse_id_from_path(req: &Request) -> Option<i64> {
    // Path shapes: /api/sources/{id} | /api/sources/{id}/articles | /api/sources/{id}/fetch
    let path = req.path();
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    parts
        .get(2)
        .and_then(|s| s.parse::<i64>().ok())
}

pub async fn update_source(mut req: Request, user_id: &str, env: &Env) -> Result<Response> {
    let id = match parse_id_from_path(&req) {
        Some(id) => id,
        None => return Response::error("invalid source id", 400),
    };
    let payload = req.json::<UpdateSourceRequest>().await?;

    let url = payload.url.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let title = payload
        .title
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let site_url = payload
        .site_url
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let enabled = payload.enabled.map(|v| v.clamp(0, 1) as i32);
    let interval = payload
        .fetch_interval_minutes
        .map(|v| v.clamp(1, 100_000) as i32);

    if url.is_none()
        && title.is_none()
        && site_url.is_none()
        && enabled.is_none()
        && interval.is_none()
    {
        return Response::error("no updates provided", 400);
    }

    let db = db::get_db(env)?;
    let args: Vec<D1Type<'_>> = vec![
        match &url {
            Some(value) => D1Type::Text(value),
            None => D1Type::Null,
        },
        match &title {
            Some(value) => D1Type::Text(value),
            None => D1Type::Null,
        },
        match &site_url {
            Some(value) => D1Type::Text(value),
            None => D1Type::Null,
        },
        enabled.map_or(D1Type::Null, D1Type::Integer),
        interval.map_or(D1Type::Null, D1Type::Integer),
        D1Type::Integer(id as i32),
        D1Type::Text(user_id),
    ];

    db.prepare(
        "UPDATE rss_sources SET
             url = COALESCE(?1, url),
             title = COALESCE(?2, title),
             site_url = COALESCE(?3, site_url),
             enabled = COALESCE(?4, enabled),
             fetch_interval_minutes = COALESCE(?5, fetch_interval_minutes),
             updated_at = CURRENT_TIMESTAMP
         WHERE id = ?6 AND user_id = ?7",
    )
    .bind_refs(args.iter())?
    .run()
    .await?;

    Response::from_json(&ApiResponse::<()> {
        success: true,
        data: None,
        error: None,
    })
}

pub async fn delete_source(req: Request, user_id: &str, env: &Env) -> Result<Response> {
    let id = match parse_id_from_path(&req) {
        Some(id) => id,
        None => return Response::error("invalid source id", 400),
    };

    let db = db::get_db(env)?;
    db.prepare("DELETE FROM rss_articles WHERE user_id = ?1 AND source_id = ?2")
        .bind(&[user_id.into(), (id as i32).into()])?
        .run()
        .await?;
    db.prepare("DELETE FROM rss_sources WHERE id = ?1 AND user_id = ?2")
        .bind(&[(id as i32).into(), user_id.into()])?
        .run()
        .await?;

    Response::from_json(&ApiResponse::<()> {
        success: true,
        data: None,
        error: None,
    })
}

pub async fn list_source_articles(req: Request, user_id: &str, env: &Env) -> Result<Response> {
    match list_source_articles_inner(&req, user_id, env).await {
        Ok(response) => Ok(response),
        Err(error) => Ok(Response::from_json(&ApiResponse::<()> {
            success: false,
            data: None,
            error: Some(format!("list articles error: {}", error)),
        })?),
    }
}

async fn list_source_articles_inner(
    req: &Request,
    user_id: &str,
    env: &Env,
) -> Result<Response> {
    let id = match parse_id_from_path(req) {
        Some(id) => id,
        None => return Response::error("invalid source id", 400),
    };

    let db = db::get_db(env)?;
    let stmt = db.prepare(
        "SELECT id, source_id, title, link, guid, summary, content, published_at, hash
         FROM rss_articles WHERE user_id = ?1 AND source_id = ?2
         ORDER BY published_at DESC LIMIT 100",
    );
    let rows = stmt.bind(&[user_id.into(), (id as i32).into()])?.all().await?;
    let articles = rows.results::<RssArticle>()?;

    Response::from_json(&ApiResponse {
        success: true,
        data: Some(articles),
        error: None,
    })
}

async fn mark_source(
    user_id: &str,
    source_id: i64,
    status: &str,
    error: Option<&str>,
    env: &Env,
) -> Result<()> {
    let db = db::get_db(env)?;
    let args: Vec<D1Type<'_>> = vec![
        D1Type::Text(status),
        match error {
            Some(message) => D1Type::Text(message),
            None => D1Type::Null,
        },
        D1Type::Integer(source_id as i32),
        D1Type::Text(user_id),
    ];
    db.prepare(
        "UPDATE rss_sources SET status = ?1, error_message = ?2,
             last_fetched_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE id = ?3 AND user_id = ?4",
    )
    .bind_refs(args.iter())?
    .run()
    .await?;
    Ok(())
}

/// Shared fetch routine for user-scoped sources (used by the queue consumer).
pub async fn process_source_job(user_id: &str, source_id: i64, url: &str, env: &Env) -> Result<()> {
    let db = db::get_db(env)?;
    let owned = db
        .prepare("SELECT id FROM rss_sources WHERE id = ?1 AND user_id = ?2")
        .bind(&[(source_id as i32).into(), user_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    if owned.is_none() {
        return Err(worker::Error::RustError("source not found for user".into()));
    }

    let parsed = match Url::parse(url) {
        Ok(parsed) => parsed,
        Err(error) => return Err(worker::Error::RustError(error.to_string())),
    };
    let mut response = crate::feed::fetch_remote(&parsed).await?;
    if !(200..300).contains(&response.status_code()) {
        let message = format!("feed returned HTTP {}", response.status_code());
        mark_source(user_id, source_id, "error", Some(&message), env).await?;
        return Err(worker::Error::RustError(message));
    }

    let content = response.text().await?;
    let articles = match crate::feed::FeedParser::parse_rss(&content, source_id as i32) {
        Ok(articles) => articles,
        Err(error) => {
            mark_source(user_id, source_id, "error", Some(&error.to_string()), env).await?;
            return Err(error);
        }
    };

    for article in articles.into_iter().take(MAX_ARTICLES_PER_FETCH) {
        let args: Vec<D1Type<'_>> = vec![
            D1Type::Text(user_id),
            D1Type::Integer(source_id as i32),
            D1Type::Text(&article.title),
            D1Type::Text(&article.link),
            D1Type::Text(&article.guid),
            d1_opt(&article.summary),
            d1_opt(&article.content),
            d1_opt(&article.published_at),
            D1Type::Text(&article.hash),
        ];
        db.prepare(
            "INSERT OR IGNORE INTO rss_articles
             (user_id, source_id, title, link, guid, summary, content, published_at, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind_refs(args.iter())?
        .run()
        .await?;
    }

    mark_source(user_id, source_id, "active", None, env).await
}

/// Manual/on-demand refresh for one user-scoped source.
pub async fn trigger_source_fetch(req: Request, user_id: &str, env: &Env) -> Result<Response> {
    match trigger_source_fetch_inner(&req, user_id, env).await {
        Ok(response) => Ok(response),
        Err(error) => Ok(Response::from_json(&ApiResponse::<()> {
            success: false,
            data: None,
            error: Some(format!("trigger fetch error: {}", error)),
        })?),
    }
}

async fn trigger_source_fetch_inner(req: &Request, user_id: &str, env: &Env) -> Result<Response> {
    let id = match parse_id_from_path(req) {
        Some(id) => id,
        None => return Response::error("invalid source id", 400),
    };

    let db = db::get_db(env)?;
    let row = db
        .prepare("SELECT url FROM rss_sources WHERE id = ?1 AND user_id = ?2")
        .bind(&[(id as i32).into(), user_id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    let url = match row {
        Some(value) => value["url"].as_str().unwrap_or("").to_string(),
        None => return Response::error("source not found", 404),
    };
    if url.is_empty() {
        return Response::error("source has no url", 400);
    }

    match process_source_job(user_id, id, &url, env).await {
        Ok(()) => {
            let count_row = db
                .prepare("SELECT COUNT(*) AS total FROM rss_articles WHERE user_id = ?1 AND source_id = ?2")
                .bind(&[user_id.into(), (id as i32).into()])?
                .first::<serde_json::Value>(None)
                .await?;
            let total = count_row.and_then(|v| v["total"].as_i64()).unwrap_or(0);
            Response::from_json(&ApiResponse {
                success: true,
                data: Some(serde_json::json!({ "source_id": id, "total": total })),
                error: None,
            })
        }
        Err(error) => Response::from_json(&ApiResponse::<()> {
            success: false,
            data: None,
            error: Some(format!("Failed to fetch source: {}", error)),
        }),
    }
}


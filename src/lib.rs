mod types;
mod routes;
mod auth;
mod sources;
mod db;
mod feed;
mod queue;
mod scheduler;
mod utils;

use worker::*;
use routes::*;

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    let method = req.method();

    // Root endpoint
    if method == Method::Get && path == "/" {
        return Response::ok(
            "Welcome to RSS Aggregator API! Available endpoints: /feed, /feed/:id, /articles",
        );
    }

    // Health check endpoint
    if path == "/health" {
        return health().await;
    }

    // Backward-compatible feed endpoints
    if method == Method::Get && path == "/feed" {
        return list_feeds(env).await;
    }

    if method == Method::Get && path.starts_with("/feed/") {
        let feed_id = path.strip_prefix("/feed/").unwrap_or("");
        if feed_id.is_empty() || feed_id.contains('/') {
            return Response::error("Missing feed ID", 400);
        }

        return Response::ok(format!("Requested feed with ID: {}", feed_id));
    }

    // API routes
    match (method, path.as_str()) {
        // Read-only production diagnostics
        (Method::Get, "/api/diagnostics") => handle_diagnostics(env).await,

        // User-scoped RSS source CRUD (/api/sources)
        (Method::Get, "/api/sources") => {
            let user_id = auth::current_user(&req);
            sources::list_sources(&user_id, &env).await
        }
        (Method::Post, "/api/sources") => {
            let user_id = auth::current_user(&req);
            sources::create_source(req, &user_id, &env).await
        }
        (Method::Post, path)
            if path.starts_with("/api/sources/") && path.ends_with("/fetch") =>
        {
            let user_id = auth::current_user(&req);
            sources::trigger_source_fetch(req, &user_id, &env).await
        }
        (Method::Get, path)
            if path.starts_with("/api/sources/") && path.ends_with("/articles") =>
        {
            let user_id = auth::current_user(&req);
            sources::list_source_articles(req, &user_id, &env).await
        }
        (Method::Put, path) if path.starts_with("/api/sources/") => {
            let user_id = auth::current_user(&req);
            sources::update_source(req, &user_id, &env).await
        }
        (Method::Delete, path) if path.starts_with("/api/sources/") => {
            let user_id = auth::current_user(&req);
            sources::delete_source(req, &user_id, &env).await
        }

        // Feed management
        (Method::Get, "/api/feeds") => handle_get_feeds(env).await,
        (Method::Post, "/api/feeds") => handle_create_feed(req, env).await,
        (Method::Delete, path) if path.starts_with("/api/feeds/") && !path.ends_with("/articles") && !path.ends_with("/subscribe") => {
            if let Ok(feed_id) = path.strip_prefix("/api/feeds/").unwrap_or("").parse::<i32>() {
                handle_delete_feed(feed_id).await
            } else {
                Response::error("Invalid feed ID", 400)
            }
        }

        // Fetch/refresh a single feed (fetch → parse → persist to D1)
        (Method::Post, path) if path.starts_with("/api/feeds/") && path.ends_with("/fetch") => {
            let feed_id_str = path
                .strip_prefix("/api/feeds/")
                .and_then(|s| s.strip_suffix("/fetch"))
                .unwrap_or("");
            if let Ok(feed_id) = feed_id_str.parse::<i32>() {
                handle_fetch_feed(feed_id, env).await
            } else {
                Response::error("Invalid feed ID", 400)
            }
        }

        // Articles
        (Method::Get, path) if path.starts_with("/api/feeds/") && path.ends_with("/articles") => {
            let feed_id_str = path
                .strip_prefix("/api/feeds/")
                .and_then(|s| s.strip_suffix("/articles"))
                .unwrap_or("");
            if let Ok(feed_id) = feed_id_str.parse::<i32>() {
                handle_get_articles(feed_id, env).await
            } else {
                Response::error("Invalid feed ID", 400)
            }
        }

        // User subscriptions
        (Method::Get, path) if path.starts_with("/api/users/") && path.ends_with("/feeds") => {
            let user_id_str = path
                .strip_prefix("/api/users/")
                .and_then(|s| s.strip_suffix("/feeds"))
                .unwrap_or("");
            if let Ok(user_id) = user_id_str.parse::<i32>() {
                handle_get_user_feeds(user_id).await
            } else {
                Response::error("Invalid user ID", 400)
            }
        }

        (Method::Post, "/api/subscriptions") => handle_subscribe_feed(req).await,
        
        (Method::Delete, path) if path.starts_with("/api/users/") && path.contains("/subscriptions/") => {
            let parts: Vec<&str> = path.split("/").collect();
            if parts.len() >= 5 {
                if let (Ok(user_id), Ok(feed_id)) = (parts[3].parse::<i32>(), parts[5].parse::<i32>()) {
                    handle_unsubscribe_feed(user_id, feed_id).await
                } else {
                    Response::error("Invalid IDs", 400)
                }
            } else {
                Response::error("Invalid path", 400)
            }
        }

        _ => Response::error("Not Found", 404),
    }
}



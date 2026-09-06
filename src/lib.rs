mod types;
mod routes;
mod db;
mod feed;
mod identity;
mod queue;
mod scheduler;
mod utils;

use worker::*;
use routes::*;

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    let method = req.method();

    // Read the Origin before `req` is moved into the handlers below — the API
    // post-processor needs it to decide CORS on every response.
    let origin = req.headers().get("Origin").ok().flatten();

    // CORS preflight (browser UI on a different origin)
    if method == Method::Options {
        let mut response = Response::empty()?;
        apply_api_headers(&mut response, origin.as_deref())?;
        return Ok(response);
    }

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
    let outcome = match (method, path.as_str()) {
        // Read-only production diagnostics + health/freshness
        (Method::Get, "/api/diagnostics") => handle_diagnostics(env).await,
        (Method::Get, "/api/health") => handle_health(env).await,

        // /api/sources — RETIRED API. The rss_sources prototype layer is
        // dormant (see ARCHITECTURE.md §3): every request here answers an
        // honest 501, never dispatches and never touches D1. Exact-surface
        // match (`==` or a `/`-suffix), not a bare prefix.
        (_, p) if p == "/api/sources" || p.starts_with("/api/sources/") => {
            handle_sources_retired().await
        }

        // Feed management
        (Method::Get, "/api/feeds") => handle_get_feeds(env).await,
        (Method::Post, "/api/feeds") => handle_create_feed(req, env).await,
        // Unsubscribe THIS device from a pool feed. When the last subscriber
        // leaves, the feed + its articles are pruned from the shared pool.
        (Method::Delete, path) if path.starts_with("/api/feeds/") && !path.ends_with("/articles") && !path.ends_with("/subscribe") && !path.ends_with("/fetch") => {
            if let Ok(feed_id) = path.strip_prefix("/api/feeds/").unwrap_or("").parse::<i32>() {
                handle_unsubscribe(feed_id, req, env).await
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

        // Per-device subscription surface (device namespace = X-User-Id header,
        // NOT authentication — see src/identity.rs). This device's list is the
        // only source the frontend nav may read (/api/feeds stays the shared
        // pool catalog, Discover-only).
        (Method::Get, "/api/me/feeds") => handle_get_my_feeds(req, env).await,

        // Subscribe THIS device to an existing shared-pool feed (Discover "+").
        (Method::Post, path) if path.starts_with("/api/feeds/") && path.ends_with("/subscribe") => {
            let feed_id_str = path
                .strip_prefix("/api/feeds/")
                .and_then(|s| s.strip_suffix("/subscribe"))
                .unwrap_or("");
            if let Ok(feed_id) = feed_id_str.parse::<i32>() {
                handle_subscribe_device(feed_id, req, env).await
            } else {
                Response::error("Invalid feed ID", 400)
            }
        }

        _ => Response::error("Not Found", 404),
    };

    let mut response = outcome?;
    apply_api_headers(&mut response, origin.as_deref())?;
    Ok(response)
}

/// Browser origins allowed to read the dynamic API cross-origin: the production
/// Pages frontend and local `astro dev`. Exact match only — no `*`, no wildcard
/// ports/hosts (a prefix/port bypass must never be admitted).
const ALLOWED_ORIGINS: &[&str] = &[
    "https://rss-intelligence.pages.dev",
    "http://localhost:4321",
    "http://127.0.0.1:4321",
];

fn is_allowed_origin(origin: &str) -> bool {
    ALLOWED_ORIGINS.contains(&origin)
}

/// Central post-processor for every dynamic `/api/**` response and the OPTIONS
/// preflight. Two jobs:
///   1. `Cache-Control: no-store` — dynamic API responses have no explicit cache
///      policy today, so browser/CDN heuristic caching is indeterminate; without
///      no-store a stale response (e.g. an early empty feed list) could
///      masquerade as the current state. Make it deterministic.
///   2. CORS — echo the request Origin only when it is on the allow-list. A
///      request without an `Origin` (curl, server-side scheduler/queue) or with
///      a disallowed one gets no `Access-Control-Allow-Origin`, so the browser
///      blocks the cross-origin read while non-browser callers are unaffected:
///      CORS is browser access control, not API authentication.
///
/// Takes the Origin header value (read in `main` before `req` is moved into
/// handlers), not the whole request.
fn apply_api_headers(response: &mut Response, origin: Option<&str>) -> Result<()> {
    response.headers_mut().set("Cache-Control", "no-store")?;
    // ACAO is chosen from the request Origin, so any cache that stores this
    // response must key on Origin too.
    response.headers_mut().set("Vary", "Origin")?;
    if let Some(origin) = origin {
        if is_allowed_origin(origin) {
            response.headers_mut().set("Access-Control-Allow-Origin", origin)?;
        }
    }
    response
        .headers_mut()
        .set("Access-Control-Allow-Headers", "Content-Type, X-User-Id")?;
    response
        .headers_mut()
        .set("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")?;
    response
        .headers_mut()
        .set("Access-Control-Max-Age", "86400")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_origins_match_exactly() {
        assert!(is_allowed_origin("https://rss-intelligence.pages.dev"));
        assert!(is_allowed_origin("http://localhost:4321"));
        assert!(is_allowed_origin("http://127.0.0.1:4321"));
    }

    #[test]
    fn disallowed_origins_are_rejected() {
        assert!(!is_allowed_origin("https://evil.example"));
        // Prefix of an allowed host must not slip through (exact match).
        assert!(!is_allowed_origin("https://sub.rss-intelligence.pages.dev"));
        assert!(!is_allowed_origin("https://rss-intelligence.pages.dev.evil.com"));
        // Wrong / missing port and unrelated hosts.
        assert!(!is_allowed_origin("http://localhost"));
        assert!(!is_allowed_origin("http://localhost:4322"));
        assert!(!is_allowed_origin("https://rss-worker-production.weixc0856.workers.dev"));
        assert!(!is_allowed_origin(""));
    }
}



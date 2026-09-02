use worker::Request;

/// Placeholder identity until a real auth provider (JWT / Cloudflare Access)
/// is wired in. Reads `X-User-Id` header, then `?user_id=` query param,
/// and finally falls back to a shared `demo` user so existing tooling works.
pub fn current_user(req: &Request) -> String {
    if let Ok(Some(value)) = req.headers().get("X-User-Id") {
        let value = value.trim();
        if !value.is_empty() {
            return value.to_string();
        }
    }
    if let Ok(url) = req.url() {
        for (key, value) in url.query_pairs() {
            if key == "user_id" && !value.is_empty() {
                return value.into_owned();
            }
        }
    }
    "demo".to_string()
}

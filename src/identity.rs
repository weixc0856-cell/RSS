//! Device identity for subscription isolation.
//!
//! The `X-User-Id` request header names a *device namespace* — one anonymous
//! browser. It is NOT authentication and NOT authorization: nothing is secret,
//! any caller may mint any key, and knowing someone else's key lets you operate
//! in their namespace. The 128-bit random UUID browsers mint is only
//! *practically collision-free* between devices, never unforgeable. Keep this
//! wording in sync with ARCHITECTURE.md §3 and migrations/007_device_profiles.sql.
//!
//! Each device key is mapped through the `profiles` table (an anonymous device
//! namespace registry, not a user/account table) to the INTEGER id that the
//! `subscriptions.user_id` column expects. A profile row is provisioned on a
//! device's first sight and is never deleted.

use worker::{Env, Error, Request, Result};

use crate::db;

/// Canonicalize a caller-supplied device key, or reject it.
///
/// Returns `None` when the header is blank or longer than 128 bytes. The cap is
/// purely an abuse guard — real device keys are 36-char UUIDs. Host-testable
/// (no D1 dependency).
pub fn normalize_device_key(raw: &str) -> Option<String> {
    let key = raw.trim();
    if key.is_empty() || key.len() > 128 {
        return None;
    }
    Some(key.to_string())
}

/// Resolve the caller's `X-User-Id` to a `profiles.id`, provisioning a row on
/// first sight. Errors with `worker::Error::RustError` when the header is
/// missing/invalid — callers map that to a `json_error(..., 400)` body.
pub async fn require_profile(req: &Request, env: &Env) -> Result<i32> {
    let raw = req
        .headers()
        .get("X-User-Id")
        .ok()
        .flatten()
        .unwrap_or_default();
    let key = normalize_device_key(&raw)
        .ok_or_else(|| Error::RustError("X-User-Id header required".to_string()))?;

    let db = db::get_db(env)?;
    profile_id_for_key(&db, &key).await
}

async fn profile_id_for_key(db: &worker::D1Database, key: &str) -> Result<i32> {
    // Fast path: this device has been seen before.
    if let Some(id) = lookup(db, key).await? {
        return Ok(id);
    }
    // Provision. Two devices racing the same brand-new key (or a retry) can
    // collide on UNIQUE(device_key) — on conflict, fall back to the lookup once.
    match insert(db, key).await {
        Ok(id) => Ok(id),
        Err(_) => lookup(db, key)
            .await?
            .ok_or_else(|| Error::RustError("profile provisioning failed".to_string())),
    }
}

async fn lookup(db: &worker::D1Database, key: &str) -> Result<Option<i32>> {
    let row = db
        .prepare("SELECT id FROM profiles WHERE device_key = ?1")
        .bind(&[key.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    Ok(row.and_then(|r| r["id"].as_i64().map(|id| id as i32)))
}

async fn insert(db: &worker::D1Database, key: &str) -> Result<i32> {
    let row = db
        .prepare("INSERT INTO profiles (device_key) VALUES (?1) RETURNING id")
        .bind(&[key.into()])?
        .first::<serde_json::Value>(None)
        .await?
        .ok_or_else(|| Error::RustError("profiles insert returned no row".to_string()))?;
    row["id"]
        .as_i64()
        .map(|id| id as i32)
        .ok_or_else(|| Error::RustError("profiles insert returned no id".to_string()))
}

#[cfg(test)]
mod tests {
    use super::normalize_device_key;

    #[test]
    fn accepts_a_plain_uuid() {
        assert_eq!(
            normalize_device_key("3f0c7d7a-8b5e-4f2a-9c1d-2e4f6a8b0c1d").as_deref(),
            Some("3f0c7d7a-8b5e-4f2a-9c1d-2e4f6a8b0c1d")
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(normalize_device_key("  abc-123  ").as_deref(), Some("abc-123"));
        assert_eq!(normalize_device_key("\tabc-123\n").as_deref(), Some("abc-123"));
    }

    #[test]
    fn rejects_blank_keys() {
        assert_eq!(normalize_device_key(""), None);
        assert_eq!(normalize_device_key("   "), None);
        assert_eq!(normalize_device_key("\t\n"), None);
    }

    #[test]
    fn rejects_overlong_keys() {
        // 129 bytes — the cap is an abuse guard, not a realistic device key.
        let long = "k".repeat(129);
        assert_eq!(normalize_device_key(&long), None);
        // Exactly at the cap is still acceptable.
        assert_eq!(normalize_device_key(&"k".repeat(128)).map(|s| s.len()), Some(128));
    }
}

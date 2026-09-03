use serde::{Deserialize, Serialize};
use worker::*;

/// Feed job body: produced by the feed-based scheduler for `type: "feed_fetch"`,
/// and by earlier (untyped) payloads that `route_job` still accepts by shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchJob {
    pub feed_id: i64,
    pub url: String,
}

/// User-scoped job body: produced for `type: "source_fetch"` (and matched by
/// shape on untyped legacy payloads). Writes `rss_articles`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceJob {
    pub source_id: i64,
    pub user_id: String,
    pub url: String,
}

/// Queue consumer for `rss-fetch-queue` / `rss-fetch-queue-prod`.
///
/// Routes each message to a handler and records the outcome on the current
/// `fetch_runs` row. Failures are recorded on the source/feed row instead of
/// retrying forever; the batch is always acked.
#[event(queue)]
pub async fn consume(
    mut batch: MessageBatch<serde_json::Value>,
    env: Env,
    _ctx: Context,
) -> Result<()> {
    let messages = batch.messages()?;
    for message in &messages {
        let raw = normalize_body(message.body().clone());
        let run_id = raw["run_id"].as_i64();

        match route_job(&raw) {
            RoutedJob::Source(job) => {
                match crate::sources::process_source_job(&job.user_id, job.source_id, &job.url, &env)
                    .await
                {
                    Ok(_) => {
                        console_log!(
                            "[queue] refreshed source_id={} user={}",
                            job.source_id,
                            job.user_id
                        );
                        record_run(&env, run_id, 1, 0, 0).await;
                    }
                    Err(error) => {
                        console_log!(
                            "[queue] source refresh failed source_id={} user={} url={}: {:?}",
                            job.source_id,
                            job.user_id,
                            job.url,
                            error
                        );
                        record_run(&env, run_id, 0, 1, 0).await;
                    }
                }
            }
            RoutedJob::Feed(job) => match crate::feed::fetch_feed(&job.url, &env).await {
                Ok(inserted) => {
                    console_log!(
                        "[queue] refreshed feed_id={} inserted={}",
                        job.feed_id,
                        inserted
                    );
                    record_run(&env, run_id, 1, 0, inserted as i64).await;
                }
                Err(error) => {
                    console_log!(
                        "[queue] refresh failed feed_id={} url={}: {:?}",
                        job.feed_id,
                        job.url,
                        error
                    );
                    record_run(&env, run_id, 0, 1, 0).await;
                }
            },
            RoutedJob::Unsupported(reason) => {
                console_log!("[queue] {reason}: {}", raw);
                // Do not process a payload we do not understand, but still count
                // it as one failed job so a run that only ever receives such
                // messages can reach a terminal state instead of lingering in
                // 'running' until it is superseded by the next cron tick.
                record_run(&env, run_id, 0, 1, 0).await;
            }
        }
    }

    batch.ack_all();
    Ok(())
}

/// How one queue message should be handled.
enum RoutedJob {
    /// User-scoped `rss_sources` job (writes `rss_articles`).
    Source(SourceJob),
    /// Legacy `feeds` job (writes `articles`).
    Feed(FetchJob),
    /// Type/version we do not understand (or a malformed payload): drop it.
    Unsupported(String),
}

/// Route a message to its handler, honouring the job payload contract.
///
/// - v1 payloads carry `type` (`feed_fetch` / `source_fetch`) and `version: 1`.
///   Any typed message whose `version` is missing or not 1 — e.g. a future v2 —
///   is `Unsupported`: it must never be processed as if it were v1. An unknown
///   `type` string is likewise `Unsupported`.
/// - Messages without `type` are the pre-contract shape and are matched by shape
///   (`source_id` + `user_id`, else `feed_id`) so already-in-flight legacy jobs
///   keep working; `version` is ignored on that path.
fn route_job(raw: &serde_json::Value) -> RoutedJob {
    let unsupported = |reason: String| RoutedJob::Unsupported(reason);

    match raw.get("type") {
        None => {
            // Legacy shape: infer the handler from the fields present.
            if raw.get("user_id").is_some() && raw.get("source_id").is_some() {
                return match serde_json::from_value::<SourceJob>(raw.clone()) {
                    Ok(job) => RoutedJob::Source(job),
                    Err(_) => unsupported(format!("malformed legacy source job: {raw}")),
                };
            }
            if raw.get("feed_id").is_some() {
                return match serde_json::from_value::<FetchJob>(raw.clone()) {
                    Ok(job) => RoutedJob::Feed(job),
                    Err(_) => unsupported(format!("malformed legacy feed job: {raw}")),
                };
            }
            unsupported(format!("unparseable job (no type field): {raw}"))
        }
        Some(serde_json::Value::String(kind)) => {
            if raw.get("version").and_then(serde_json::Value::as_i64) != Some(1) {
                let version = raw
                    .get("version")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "missing".to_string());
                return unsupported(format!(
                    "unsupported job version={version} type={kind}: {raw}"
                ));
            }
            match kind.as_str() {
                "source_fetch" => match serde_json::from_value::<SourceJob>(raw.clone()) {
                    Ok(job) => RoutedJob::Source(job),
                    Err(_) => unsupported(format!("malformed source_fetch job: {raw}")),
                },
                "feed_fetch" => match serde_json::from_value::<FetchJob>(raw.clone()) {
                    Ok(job) => RoutedJob::Feed(job),
                    Err(_) => unsupported(format!("malformed feed_fetch job: {raw}")),
                },
                other => unsupported(format!("unsupported job type={other}: {raw}")),
            }
        }
        Some(_) => unsupported(format!("malformed job type field: {raw}")),
    }
}

/// workerd delivers string-typed queue bodies verbatim (it does not re-parse
/// them even when the producer sent `contentType: json`). worker-rs therefore
/// surfaces e.g. `"{\"feed_id\":3,...}"` as a JSON *string* body. Normalise it
/// back to a JSON object so both object and string payloads match the
/// `SourceJob` / `FetchJob` shapes.
fn normalize_body(body: serde_json::Value) -> serde_json::Value {
    match body {
        serde_json::Value::String(text) => {
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
        }
        other => other,
    }
}

/// Accumulate the outcome of one queued job onto its `fetch_runs` row and mark
/// the run finished once every scheduled job has reported back.
///
/// Cumulative totals after this job are the ones that decide the terminal state:
///   new_fetched = feeds_fetched + {fetched}
///   new_failed  = feeds_failed  + {failed}
/// and the run is finished once `new_fetched + new_failed >= feeds_scheduled`.
///
/// The `finished_at` / `status` CASE expressions below read the columns *after*
/// the increment lines, deliberately relying on SQLite evaluating UPDATE `SET`
/// clauses left-to-right (a later expression observes the value assigned by an
/// earlier one). This is an intentional dependency, not an accident: it lets a
/// single atomic statement classify the run from the new totals — equivalent to
/// `classify_run(feeds_scheduled, new_fetched, new_failed)` — without a separate
/// read that could race another job reporting on the same run. Keep this SQL in
/// sync with `classify_run`.
///
/// Numbers are interpolated as integer literals on purpose: the D1/worker-rs
/// parameter binding has silently dropped bound args on this hot path before,
/// and every interpolated value is an i64 this function produced (never user
/// input), so there is no injection surface.
async fn record_run(
    env: &Env,
    run_id: Option<i64>,
    fetched: i64,
    failed: i64,
    inserted: i64,
) {
    let Some(run_id) = run_id else { return };
    let db = match crate::db::get_db(env) {
        Ok(db) => db,
        Err(_) => return,
    };
    let sql = format!(
        "UPDATE fetch_runs SET
            feeds_fetched = feeds_fetched + {fetched},
            feeds_failed = feeds_failed + {failed},
            articles_inserted = articles_inserted + {inserted},
            finished_at = CASE
                WHEN feeds_scheduled > 0 AND feeds_fetched + feeds_failed >= feeds_scheduled
                THEN datetime('now') ELSE finished_at END,
            status = CASE
                WHEN feeds_scheduled > 0 AND feeds_fetched + feeds_failed >= feeds_scheduled
                THEN CASE
                    WHEN feeds_failed > 0 AND feeds_fetched = 0 THEN 'failed'
                    WHEN feeds_failed > 0 THEN 'partial'
                    ELSE 'ok'
                END
                ELSE status END
            WHERE id = {run_id} AND status = 'running'"
    );
    let _ = db.prepare(&sql).run().await;
}

/// Terminal status of a scheduler run once all scheduled jobs have reported, or
/// `None` while the run is still in flight (`feeds_scheduled` not yet set, or
/// fewer than `scheduled` jobs have reported back).
///
/// Uses `>=`: late, duplicate or retried deliveries can push cumulative counts
/// past `scheduled`; that must still terminate the run, never re-open it, and a
/// run with any failure must never classify as `ok` even if the last job
/// reported back clean.
///
/// This is the specification mirror of the SQL CASE in `record_run` — change one
/// and change the other.
fn classify_run(scheduled: i64, fetched: i64, failed: i64) -> Option<&'static str> {
    if scheduled <= 0 || fetched + failed < scheduled {
        return None;
    }
    if failed > 0 {
        Some(if fetched == 0 { "failed" } else { "partial" })
    } else {
        Some("ok")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_job_serializes_and_deserializes() {
        let job = FetchJob {
            feed_id: 42,
            url: "https://example.com/rss.xml".to_string(),
        };
        let json = serde_json::to_string(&job).expect("serialize");
        let back: FetchJob = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(job.feed_id, back.feed_id);
        assert_eq!(job.url, back.url);
    }

    #[test]
    fn fetch_job_parses_from_scheduler_payload() {
        // Mirrors the exact v1 payload produced by scheduler.rs (extra version /
        // type fields are ignored by the struct-level deserializer).
        let payload = serde_json::json!({
            "version": 1,
            "type": "feed_fetch",
            "feed_id": 7,
            "url": "https://example.com/rss",
            "run_id": 99
        });
        let job: FetchJob = serde_json::from_value(payload).expect("parse");
        assert_eq!(job.feed_id, 7);
        assert_eq!(job.url, "https://example.com/rss");
    }

    #[test]
    fn fetch_job_rejects_malformed_payloads() {
        // Missing url / wrong types must not silently produce a bogus job.
        assert!(serde_json::from_str::<FetchJob>(r#"{"feed_id":1}"#).is_err());
        assert!(
            serde_json::from_str::<FetchJob>(r#"{"feed_id":"x","url":"https://a.b"}"#).is_err()
        );
    }

    #[test]
    fn source_job_round_trips_and_parses_scheduler_payload() {
        let json = r#"{"source_id":9,"user_id":"alice","url":"https://example.com/rss"}"#;
        let job: SourceJob = serde_json::from_str(json).expect("parse source job");
        assert_eq!(job.source_id, 9);
        assert_eq!(job.user_id, "alice");
        assert_eq!(job.url, "https://example.com/rss");

        let serialized = serde_json::to_string(&job).expect("serialize");
        let back: SourceJob = serde_json::from_str(&serialized).expect("round trip");
        assert_eq!(back.source_id, job.source_id);
        assert_eq!(back.user_id, job.user_id);
    }

    #[test]
    fn source_job_rejects_malformed_payloads() {
        assert!(serde_json::from_str::<SourceJob>(r#"{"source_id":1,"url":"x"}"#).is_err());
        assert!(
            serde_json::from_str::<SourceJob>(r#"{"source_id":"x","user_id":"a","url":"x"}"#)
                .is_err()
        );
    }

    #[test]
    fn normalize_body_parses_embedded_json_string() {
        // Mirrors how workerd delivers string bodies verbatim, e.g. a producer
        // that sent `"{\"feed_id\":7,\"url\":\"https://a.b/rss\"}"`.
        let body = serde_json::json!("{\"feed_id\":7,\"url\":\"https://a.b/rss\"}");
        let out = normalize_body(body);
        assert_eq!(out["feed_id"], 7);
        assert_eq!(out["url"], "https://a.b/rss");
    }

    #[test]
    fn normalize_body_keeps_non_json_strings() {
        let body = serde_json::json!("plain message");
        assert_eq!(normalize_body(body), serde_json::json!("plain message"));
    }

    #[test]
    fn normalize_body_leaves_objects_untouched() {
        let body = serde_json::json!({ "feed_id": 3, "url": "https://x" });
        assert_eq!(normalize_body(body.clone()), body);
    }

    // ---- route_job: typed v1 contract ---------------------------------------

    #[test]
    fn route_job_accepts_versioned_feed_fetch() {
        let raw = serde_json::json!({
            "version": 1,
            "type": "feed_fetch",
            "feed_id": 7,
            "url": "https://example.com/rss",
            "run_id": 1
        });
        match route_job(&raw) {
            RoutedJob::Feed(job) => assert_eq!(job.feed_id, 7),
            other => panic!("expected Feed, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn route_job_accepts_versioned_source_fetch() {
        let raw = serde_json::json!({
            "version": 1,
            "type": "source_fetch",
            "source_id": 9,
            "user_id": "alice",
            "url": "https://example.com/rss",
            "run_id": 1
        });
        match route_job(&raw) {
            RoutedJob::Source(job) => assert_eq!(job.source_id, 9),
            other => panic!("expected Source, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn route_job_rejects_unknown_version() {
        // A future v2 must be explicitly rejected, never processed as v1.
        for version in [2, 0] {
            let raw = serde_json::json!({
                "version": version,
                "type": "feed_fetch",
                "feed_id": 7,
                "url": "https://example.com/rss"
            });
            assert!(
                matches!(route_job(&raw), RoutedJob::Unsupported(ref r) if r.contains("version"))
            );
        }
    }

    #[test]
    fn route_job_rejects_typed_message_without_version() {
        // `type` present but `version` missing is not a valid v1 message.
        let raw = serde_json::json!({ "type": "feed_fetch", "feed_id": 7, "url": "x" });
        assert!(matches!(route_job(&raw), RoutedJob::Unsupported(_)));
    }

    #[test]
    fn route_job_rejects_unknown_type() {
        let raw = serde_json::json!({
            "version": 1,
            "type": "migrate_feed",
            "feed_id": 7,
            "url": "x"
        });
        assert!(
            matches!(route_job(&raw), RoutedJob::Unsupported(ref r) if r.contains("type=migrate_feed"))
        );
    }

    #[test]
    fn route_job_rejects_malformed_typed_payload() {
        // Correct type/version but missing required fields is still unsupported.
        let raw = serde_json::json!({ "version": 1, "type": "feed_fetch", "feed_id": 7 });
        assert!(matches!(route_job(&raw), RoutedJob::Unsupported(_)));
    }

    // ---- route_job: legacy (untyped) shape fallback --------------------------

    #[test]
    fn route_job_legacy_feed_shape_routes_by_feed_id() {
        // Pre-contract payloads have no `type`; keep dispatching by shape so
        // already-in-flight messages are not dropped.
        let raw = serde_json::json!({ "feed_id": 3, "url": "https://a.b/rss", "run_id": 5 });
        assert!(matches!(route_job(&raw), RoutedJob::Feed(_)));
    }

    #[test]
    fn route_job_legacy_source_shape_routes_by_source_and_user() {
        let raw = serde_json::json!({
            "source_id": 2,
            "user_id": "bob",
            "url": "https://a.b/rss",
            "run_id": 5
        });
        assert!(matches!(route_job(&raw), RoutedJob::Source(_)));
    }

    #[test]
    fn route_job_drops_unparseable_legacy_jobs() {
        assert!(matches!(
            route_job(&serde_json::json!({ "url": "https://a.b/rss" })),
            RoutedJob::Unsupported(_)
        ));
        assert!(matches!(
            route_job(&serde_json::json!({ "feed_id": 3 })), // missing url
            RoutedJob::Unsupported(_)
        ));
    }

    // ---- classify_run --------------------------------------------------------

    #[test]
    fn classify_run_returns_none_while_in_flight() {
        assert_eq!(classify_run(3, 0, 0), None); // scheduled but nothing back yet
        assert_eq!(classify_run(3, 2, 0), None); // still waiting on one more
        assert_eq!(classify_run(0, 0, 0), None); // feeds_scheduled never set
    }

    #[test]
    fn classify_run_all_success_is_ok() {
        assert_eq!(classify_run(2, 2, 0), Some("ok"));
        assert_eq!(classify_run(3, 3, 0), Some("ok"));
    }

    #[test]
    fn classify_run_any_failure_is_partial_or_failed() {
        // Last job succeeded but an earlier one failed -> still partial, not ok.
        assert_eq!(classify_run(2, 1, 1), Some("partial"));
        assert_eq!(classify_run(2, 2, 1), Some("partial"));
        assert_eq!(classify_run(3, 1, 2), Some("partial"));
        // No job succeeded at all -> failed.
        assert_eq!(classify_run(2, 0, 2), Some("failed"));
    }

    #[test]
    fn classify_run_over_count_never_flips_back_to_ok() {
        // Duplicate / late deliveries push cumulative counts past `scheduled`;
        // >= must still terminate the run and must not erase earlier failures.
        assert_eq!(classify_run(2, 2, 1), Some("partial")); // cumulative 3 >= 2
        assert_eq!(classify_run(2, 3, 0), Some("ok")); // cumulative 3 >= 2, none failed
        assert_eq!(classify_run(1, 1, 1), Some("partial")); // cumulative 2 >= 1
    }
}

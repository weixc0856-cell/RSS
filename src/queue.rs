use serde::{Deserialize, Serialize};
use worker::*;

/// Legacy payload produced by the feed-based scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchJob {
    pub feed_id: i64,
    pub url: String,
}

/// User-scoped payload produced by the source-based scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceJob {
    pub source_id: i64,
    pub user_id: String,
    pub url: String,
}

/// Queue consumer for `rss-fetch-queue` / `rss-fetch-queue-prod`.
///
/// Routes each message by shape:
/// - `source_id` → user-scoped `rss_sources` job (writes `rss_articles`)
/// - `feed_id`   → legacy feed job (writes `articles`)
/// Failures are recorded on the source/feed row instead of retrying forever.
#[event(queue)]
pub async fn consume(
    mut batch: MessageBatch<serde_json::Value>,
    env: Env,
    _ctx: Context,
) -> Result<()> {
    let messages = batch.messages()?;
    for message in &messages {
        let raw = message.body().clone();

        if let Ok(job) = serde_json::from_value::<SourceJob>(raw.clone()) {
            match crate::sources::process_source_job(&job.user_id, job.source_id, &job.url, &env)
                .await
            {
                Ok(_) => {
                    console_log!("[queue] refreshed source_id={} user={}", job.source_id, job.user_id)
                }
                Err(error) => console_log!(
                    "[queue] source refresh failed source_id={} user={} url={}: {:?}",
                    job.source_id,
                    job.user_id,
                    job.url,
                    error
                ),
            }
            continue;
        }

        if let Ok(job) = serde_json::from_value::<FetchJob>(raw.clone()) {
            match crate::feed::fetch_feed(&job.url, &env).await {
                Ok(_) => console_log!("[queue] refreshed feed_id={}", job.feed_id),
                Err(error) => console_log!(
                    "[queue] refresh failed feed_id={} url={}: {:?}",
                    job.feed_id,
                    job.url,
                    error
                ),
            }
            continue;
        }

        console_log!("[queue] dropping unparseable job: {}", raw);
    }

    batch.ack_all();
    Ok(())
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
        // Mirrors the exact payload produced by scheduler.rs.
        let payload = serde_json::json!({ "feed_id": 7, "url": "https://example.com/rss" });
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
}

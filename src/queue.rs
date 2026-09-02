use serde::{Deserialize, Serialize};
use worker::*;

/// Payload contract between `scheduler` (producer) and this consumer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchJob {
    pub feed_id: i64,
    pub url: String,
}

/// Queue consumer for `rss-fetch-queue` (see `[[queues.consumers]]`).
///
/// Each message is a `FetchJob` produced by `scheduler::run`. The real work is
/// delegated to `feed::fetch_feed`, which fetches the origin, parses the
/// RSS/Atom document, persists new articles to D1 and updates the feed status
/// (including `error_message`). Messages are acknowledged after processing;
/// transient origin failures are recorded on the feed row instead of being
/// retried forever.
#[event(queue)]
pub async fn consume(
    mut batch: MessageBatch<serde_json::Value>,
    env: Env,
    _ctx: Context,
) -> Result<()> {
    let messages = batch.messages()?;
    for message in &messages {
        let job: FetchJob = match serde_json::from_value(message.body().clone()) {
            Ok(job) => job,
            Err(error) => {
                console_log!(
                    "[queue] dropping unparseable job: {} (err: {:?})",
                    message.body(),
                    error
                );
                continue;
            }
        };

        match crate::feed::fetch_feed(&job.url, &env).await {
            Ok(_) => console_log!("[queue] refreshed feed_id={}", job.feed_id),
            Err(error) => console_log!(
                "[queue] refresh failed feed_id={} url={}: {:?}",
                job.feed_id,
                job.url,
                error
            ),
        }
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
}

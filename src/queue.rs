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

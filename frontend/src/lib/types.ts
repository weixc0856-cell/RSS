/** Shared data shapes mirroring the Worker REST API. */

export interface ApiResponse<T> {
  success: boolean;
  data: T | null;
  error: string | null;
}

export interface Feed {
  id: number;
  url: string;
  title: string | null;
  site_url?: string | null;
  favicon_url?: string | null;
  last_fetched_at: string | null;
  status: string;
  error_message?: string | null;
  enabled?: number;
  fetch_interval_minutes?: number;
  last_success_at?: string | null;
  last_failure_at?: string | null;
  last_http_status?: number | null;
  consecutive_failures?: number;
  next_fetch_at?: string | null;
  normalized_url?: string | null;
  created_at?: string | null;
  updated_at?: string | null;
  /** Only present on rows from /api/me/feeds (the per-device list). */
  subscribed_at?: string | null;
  /** Only present on rows from /api/me/feeds. */
  article_count?: number;
}

/** POST /api/feeds (find-or-create + subscribe THIS device) response data.
 *  `created` = brand-new in the shared pool; `already` = this device was
 *  already subscribed before this call. Drives honest add-form copy. */
export interface AddFeedResult {
  feed: Feed;
  created: boolean;
  already: boolean;
}

/** DELETE /api/feeds/:id (unsubscribe THIS device) response data.
 *  `pruned` = this was the last subscriber, so the feed + its articles were
 *  removed from the shared pool too. */
export interface DeleteResult {
  id: number;
  pruned: boolean;
}

export interface Article {
  id: number;
  feed_id: number;
  title: string;
  link: string;
  guid: string;
  summary: string | null;
  content: string | null;
  published_at: string | null;
  hash: string;
}

export interface StatusCount {
  status: string;
  c: number;
}

export interface FailedFeed {
  id: number;
  title: string | null;
  url: string;
  error_message: string | null;
  last_fetched_at: string | null;
  last_failure_at?: string | null;
  last_http_status?: number | null;
  consecutive_failures?: number;
  next_fetch_at?: string | null;
}

export interface CronTicks {
  ticks: number;
  last_tick: string | null;
}

export interface FetchRun {
  id?: number;
  started_at?: string | null;
  finished_at?: string | null;
  trigger?: string;
  feeds_scheduled?: number;
  feeds_fetched?: number;
  feeds_failed?: number;
  articles_inserted?: number;
  status?: string;
}

export interface Diagnostics {
  feeds_by_status: StatusCount[];
  articles_total: { total: number }[];
  failed_feeds: FailedFeed[];
  cron_ticks: CronTicks[];
  last_fetch_run?: FetchRun;
  generated_at: string;
}

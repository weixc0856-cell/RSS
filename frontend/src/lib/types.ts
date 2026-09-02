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
}

export interface CronTicks {
  ticks: number;
  last_tick: string | null;
}

export interface Diagnostics {
  feeds_by_status: StatusCount[];
  articles_total: { total: number }[];
  failed_feeds: FailedFeed[];
  cron_ticks: CronTicks[];
  generated_at: string;
}

export type EnvKey = "dev" | "prod";

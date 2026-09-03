import type {
  ApiResponse,
  Article,
  Diagnostics,
  Feed,
} from "./types";

/**
 * API layer: the ONLY place that talks to the Cloudflare Worker.
 * UI components never construct fetch calls themselves (data/design separation).
 *
 * Production data-plane rule: the production Worker (`rss-worker-production`)
 * backed by the `rss-db` D1 database is the single source of truth. The build
 * may override it only through the deployment config (not per-user UI state).
 */
export const API_BASE: string =
  import.meta.env.ASTRO_PUBLIC_API_BASE ??
  "https://rss-worker-production.weixc0856.workers.dev";

export type ApiErrorCode = "network" | "http";

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status?: number,
    readonly code?: ApiErrorCode
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(
  path: string,
  init?: RequestInit
): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`${API_BASE}${path}`, {
      headers: { "Content-Type": "application/json" },
      ...init,
    });
  } catch {
    throw new ApiError(`Network error reaching ${API_BASE}${path}`, undefined, "network");
  }

  let json: ApiResponse<T>;
  try {
    json = (await res.json()) as ApiResponse<T>;
  } catch {
    throw new ApiError(`Invalid JSON from ${res.status}`, res.status, "http");
  }

  if (!json.success || json.error) {
    throw new ApiError(
      json.error ?? `Request failed (${res.status})`,
      res.status,
      "http"
    );
  }
  return json.data as T;
}

export async function getFeeds(): Promise<Feed[]> {
  return request<Feed[]>("/api/feeds");
}

export async function getArticles(feedId: number): Promise<Article[]> {
  return request<Article[]>(`/api/feeds/${feedId}/articles`);
}

export async function getDiagnostics(): Promise<Diagnostics> {
  return request<Diagnostics>("/api/diagnostics");
}

export async function triggerFetch(feedId: number): Promise<{ total: number }> {
  return request<{ total: number }>(`/api/feeds/${feedId}/fetch`, {
    method: "POST",
  });
}

export async function addFeed(url: string, title: string): Promise<Feed> {
  return request<Feed>("/api/feeds", {
    method: "POST",
    body: JSON.stringify({ url, title }),
  });
}

/** Human friendly relative time from an RFC3339/RSS date string. */
export function timeAgo(value: string | null): string {
  if (!value) return "unknown";
  const t = new Date(value).getTime();
  if (Number.isNaN(t)) return "just now";
  const diff = Date.now() - t;
  const s = Math.floor(diff / 1000);
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  return `${d}d ago`;
}

/** Human friendly relative time until a future date string ("in 3m"). */
export function timeUntil(value: string | null): string {
  if (!value) return "soon";
  const t = new Date(value).getTime();
  if (Number.isNaN(t)) return "soon";
  const diff = t - Date.now();
  if (diff <= 60_000) return "in <1m";
  const m = Math.floor(diff / 60_000);
  if (m < 60) return `in ${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `in ${h}h`;
  return `in ${Math.floor(h / 24)}d`;
}

export function domainOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

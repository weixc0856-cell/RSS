import type {
  AddFeedResult,
  ApiResponse,
  Article,
  DeleteResult,
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
 *
 * Device model: each browser owns an independent subscription list. The random
 * key minted below travels as the `X-User-Id` header on every request. It names
 * a device *namespace*, NOT a login: anyone may mint any key, and collisions
 * are merely improbable, never impossible (see ARCHITECTURE.md §3). Where a
 * key is stored is per-browser only — there is no cross-device sync.
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

const DEVICE_KEY_STORAGE = "rss_device_key";
let memoryKey: string | null = null;

function makeDeviceKey(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    try {
      return crypto.randomUUID();
    } catch {
      // fall through to the non-crypto fallback
    }
  }
  // Non-secure context or crypto unavailable: still a unique-enough namespace
  // key for isolation (collision probability, not a security boundary).
  return `dev-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;
}

/**
 * This browser's stable device key, minted on first call and persisted in
 * localStorage. localStorage can be disabled (private mode, blocked storage) —
 * then the key lives only for this page session and a reload looks like a new
 * device, which the model tolerates (isolation is per-browser by design).
 */
export function getDeviceKey(): string {
  if (memoryKey) return memoryKey;
  try {
    const stored = window.localStorage.getItem(DEVICE_KEY_STORAGE);
    if (stored) {
      memoryKey = stored;
      return stored;
    }
  } catch {
    // localStorage unavailable — fall through to an in-memory key.
  }
  const fresh = makeDeviceKey();
  memoryKey = fresh;
  try {
    window.localStorage.setItem(DEVICE_KEY_STORAGE, fresh);
  } catch {
    // Keep the in-memory key for this session.
  }
  return fresh;
}

function baseHeaders(): Record<string, string> {
  // The custom X-User-Id header makes every request preflight; the worker
  // re-advertises it in Access-Control-Allow-Headers.
  return {
    "Content-Type": "application/json",
    "X-User-Id": getDeviceKey(),
  };
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`${API_BASE}${path}`, {
      headers: baseHeaders(),
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

/** Shared-pool catalog — DISCOVER-ONLY. The feed nav MUST NEVER derive from
 *  this list (invariant, see ARCHITECTURE.md): a new device has its own empty
 *  list while the pool stays full. Feed the Discover strip from here, the nav
 *  from getMyFeeds(). */
export async function getFeeds(): Promise<Feed[]> {
  return request<Feed[]>("/api/feeds");
}

/** THIS device's feed list — NAVIGATION-ONLY. The only source the sidebar nav
 *  may read. Same feed projection as /api/feeds plus subscribed_at and
 *  article_count. Requires the X-User-Id header. */
export async function getMyFeeds(): Promise<Feed[]> {
  return request<Feed[]>("/api/me/feeds");
}

/** Subscribe THIS device to an existing shared-pool feed (Discover "+").
 *  Idempotent. */
export async function subscribeFeed(feedId: number): Promise<Feed> {
  return request<Feed>(`/api/feeds/${feedId}/subscribe`, {
    method: "POST",
  });
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

/** Find-or-create a shared-pool feed for this URL and subscribe THIS device.
 *  Idempotent, always success on a valid body — created/already tell the story. */
export async function addFeed(url: string, title: string): Promise<AddFeedResult> {
  return request<AddFeedResult>("/api/feeds", {
    method: "POST",
    body: JSON.stringify({ url, title }),
  });
}

/** Unsubscribe THIS device from a pool feed. When the last subscriber leaves,
 *  the worker also prunes the feed + its articles from the shared pool
 *  (pruned: true). */
export async function deleteFeed(feedId: number): Promise<DeleteResult> {
  return request<DeleteResult>(`/api/feeds/${feedId}`, {
    method: "DELETE",
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

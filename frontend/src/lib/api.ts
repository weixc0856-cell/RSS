import type {
  ApiResponse,
  Article,
  Diagnostics,
  EnvKey,
  Feed,
} from "./types";

/**
 * API layer: the ONLY place that talks to the Cloudflare Worker.
 * UI components never construct fetch calls themselves (data/design separation).
 */
export const API_BASES: Record<EnvKey, string> = {
  dev:
    import.meta.env.ASTRO_PUBLIC_API_DEV ??
    "https://rss-worker.weixc0856.workers.dev",
  prod:
    import.meta.env.ASTRO_PUBLIC_API_PROD ??
    "https://rss-worker-production.weixc0856.workers.dev",
};

export class ApiError extends Error {
  constructor(message: string, readonly status?: number) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(
  base: string,
  path: string,
  init?: RequestInit
): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`${base}${path}`, {
      headers: { "Content-Type": "application/json" },
      ...init,
    });
  } catch (cause) {
    throw new ApiError(`Network error reaching ${base}${path}`);
  }

  let json: ApiResponse<T>;
  try {
    json = (await res.json()) as ApiResponse<T>;
  } catch {
    throw new ApiError(`Invalid JSON from ${res.status}`, res.status);
  }

  if (!json.success || json.error) {
    throw new ApiError(json.error ?? `Request failed (${res.status})`, res.status);
  }
  return json.data as T;
}

export async function getFeeds(env: EnvKey): Promise<Feed[]> {
  return request<Feed[]>(API_BASES[env], "/api/feeds");
}

export async function getArticles(env: EnvKey, feedId: number): Promise<Article[]> {
  return request<Article[]>(API_BASES[env], `/api/feeds/${feedId}/articles`);
}

export async function getDiagnostics(env: EnvKey): Promise<Diagnostics> {
  return request<Diagnostics>(API_BASES[env], "/api/diagnostics");
}

export async function triggerFetch(env: EnvKey, feedId: number): Promise<{ total: number }> {
  return request<{ total: number }>(
    API_BASES[env],
    `/api/feeds/${feedId}/fetch`,
    { method: "POST" }
  );
}

export async function addFeed(
  env: EnvKey,
  url: string,
  title: string
): Promise<Feed> {
  return request<Feed>(API_BASES[env], "/api/feeds", {
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

export function domainOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

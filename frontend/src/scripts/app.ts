import {
  ApiError,
  addFeed,
  deleteFeed,
  domainOf,
  getArticles,
  getDiagnostics,
  getFeeds,
  getMyFeeds,
  subscribeFeed,
  timeAgo,
  timeUntil,
  triggerFetch,
} from "../lib/api";
import type { Article, Diagnostics, Feed } from "../lib/types";
import { initMotion, refreshMotion } from "./animate";

/** App controller: owns UI state and rendering. Data access only via lib/api.
 *
 *  Device model (see ARCHITECTURE.md §3): `feeds` below is THIS device's list
 *  (GET /api/me/feeds) — the only source the nav may read. `poolFeeds` is the
 *  shared-pool catalog (GET /api/feeds), Discover-only: it feeds the one-click
 *  suggestion strip and is NEVER shown as this device's feeds. New devices
 *  start empty and subscribe from Discover or by pasting a feed URL. */

function $<T extends HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Missing #${id}`);
  return el as T;
}

const els = {
  feedNav: $<HTMLElement>("feedNav"),
  discover: $<HTMLElement>("discover"),
  healthDot: $<HTMLElement>("healthDot"),
  healthText: $<HTMLElement>("healthText"),
  subtitle: $<HTMLElement>("subtitle"),
  stats: $<HTMLElement>("stats"),
  items: $<HTMLElement>("items"),
  empty: $<HTMLElement>("empty"),
  search: $<HTMLInputElement>("search"),
  sort: $<HTMLSelectElement>("sort"),
  refresh: $<HTMLButtonElement>("refresh"),
  feedForm: $<HTMLFormElement>("feedForm"),
  feedUrl: $<HTMLInputElement>("feedUrl"),
  feedTitle: $<HTMLInputElement>("feedTitle"),
  toast: $<HTMLElement>("toast"),
};

type LoadState = "loading" | "loaded" | "error";

let feeds: Feed[] = [];
/** Shared-pool catalog for the Discover strip only — never this device's list. */
let poolFeeds: Feed[] = [];
let articles: Article[] = [];
let activeFeedId: number | null = null;
let diagnostics: Diagnostics | null = null;
let toastTimer = 0;

// Per-area load state. "loading" and "error" must never render as an empty
// feed/article list — only a successful load with zero rows means "no data".
let feedsState: LoadState = "loading";
let articlesState: LoadState = "loading";
let diagOk = false;

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function toast(message: string, isError = false): void {
  els.toast.textContent = message;
  els.toast.classList.toggle("error", isError);
  els.toast.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => els.toast.classList.remove("show"), 3200);
}

function feedById(id: number): Feed | undefined {
  return feeds.find((f) => f.id === id);
}

interface HealthLine {
  tone: "ok" | "err" | "warn";
  badge: string;
  detail: string;
}

/** Derive the per-feed health line from the fields the feed endpoints send. */
function feedHealth(f: Feed): HealthLine {
  if (f.status === "error") {
    const code = f.last_http_status ? `HTTP ${f.last_http_status}` : "HTTP ?";
    const retries =
      (f.consecutive_failures ?? 0) > 0 ? ` · ×${f.consecutive_failures}` : "";
    return {
      tone: "err",
      badge: `Failed · ${code}`,
      detail: `retry ${timeUntil(f.next_fetch_at ?? null)}${retries}`,
    };
  }
  if (f.status === "active") {
    return {
      tone: "ok",
      badge: "Healthy",
      detail: `last ${timeAgo(f.last_success_at ?? f.last_fetched_at ?? null)}`,
    };
  }
  return {
    tone: "warn",
    badge: f.status || "Queued",
    detail: "never fetched",
  };
}

function renderNav(): void {
  if (feedsState === "loading") {
    els.feedNav.innerHTML =
      `<div class="nav-state"><span class="spinner"></span>Loading feeds…</div>`;
    return;
  }
  if (feedsState === "error") {
    els.feedNav.innerHTML =
      `<div class="nav-state error">
        <div class="nav-state-title">Unable to load feeds</div>
        <div class="nav-state-sub">Production API unavailable.</div>
        <button class="action-btn" type="button" data-action="retry-feeds">Retry</button>
      </div>`;
    return;
  }
  const html = feeds
    .map((f) => {
      const title = escapeHtml(f.title || domainOf(f.url));
      const health = feedHealth(f);
      const dotCls = `dot ${health.tone}`;
      const metaErr = health.tone === "err" ? "err" : "";
      const active = f.id === activeFeedId ? "active" : "";
      const tip = `${health.badge} · ${health.detail}`;
      // A feed row is a wrapper div holding the select <button> plus a sibling
      // remove <button> — buttons cannot nest, so the ✕ lives outside .nav-item.
      // `title`/`tip` here are already escaped: safe to inject into attributes.
      return `<div class="feed-item ${active}">
        <button class="nav-item ${active}" type="button" data-feed="${f.id}" title="${escapeHtml(
        tip
      )}">
          <span class="nav-row">
            <span class="${dotCls}"></span><span class="nav-title">${title}</span>
          </span>
          <span class="nav-meta ${metaErr}">
            <span class="health-badge">${escapeHtml(health.badge)}</span>
            <span>${escapeHtml(health.detail)}</span>
          </span>
        </button>
        <button class="nav-remove" type="button" data-action="remove-feed" data-feed="${f.id}"
                title="Remove ${title} from this device" aria-label="Remove ${title} from this device">✕</button>
      </div>`;
    })
    .join("");
  els.feedNav.innerHTML =
    html ||
    `<div class="nav-state">No feeds on this device yet<div class="nav-state-sub">Add from Discover below or paste a feed URL.</div></div>`;
}

function renderStats(): void {
  const stat = (value: string | number, label: string): string =>
    `<div class="stat"><div class="value">${value}</div><div class="label">${label}</div></div>`;

  // Active/failed/article numbers are THIS DEVICE's (derived from its list),
  // never the shared pool's global tallies — a new device must show 0, not the
  // pool's 5 feeds. Diagnostics is auxiliary and supplies only the sync line
  // (the cron run is shared by every device); when it fails those numbers fall
  // back to "—" rather than to pool counts.
  const active = feeds.filter((f) => f.status === "active").length;
  const errors = feeds.filter((f) => f.status === "error").length;
  const articles = feeds.reduce((n, f) => n + (f.article_count ?? 0), 0);
  els.healthDot.classList.toggle("error", errors > 0);

  if (diagOk && diagnostics) {
    const run = diagnostics.last_fetch_run;
    const lastSync = run?.started_at ?? diagnostics.cron_ticks[0]?.last_tick ?? null;
    const runFailed = run?.feeds_failed ?? 0;
    els.healthText.textContent = `${active} active · ${errors} failed · sync ${timeAgo(
      lastSync
    )}${runFailed ? ` · ${runFailed} failed this run` : ""}`;
    els.subtitle.textContent = `${active} live feeds · ${articles} articles stored`;

    els.stats.innerHTML =
      stat(active, "Active feeds") +
      stat(errors, "Failed feeds") +
      stat(articles, "Articles") +
      stat(timeAgo(lastSync), "Last sync");
  } else {
    els.healthText.textContent = "System status unavailable";
    els.subtitle.textContent = `${active} live feeds`;

    els.stats.innerHTML =
      stat(active, "Active feeds") +
      stat(errors, "Failed feeds") +
      stat("—", "Articles") +
      stat("—", "Last sync");
  }
}

function visibleArticles(): Article[] {
  const q = els.search.value.trim().toLowerCase();
  let list = articles;
  if (q) {
    list = list.filter(
      (a) =>
        a.title.toLowerCase().includes(q) ||
        (a.summary ?? "").toLowerCase().includes(q) ||
        domainOf(a.link).includes(q)
    );
  }
  const asc = els.sort.value === "oldest";
  return list.sort((a, b) => {
    const ta = new Date(a.published_at ?? 0).getTime() || 0;
    const tb = new Date(b.published_at ?? 0).getTime() || 0;
    return asc ? ta - tb : tb - ta;
  });
}

function renderItems(): void {
  if (articlesState === "error") {
    els.empty.classList.remove("visible");
    els.items.innerHTML =
      `<div class="items-state error">
        <div class="nav-state-title">Unable to load articles</div>
        <button class="action-btn" type="button" data-action="retry-articles">Retry</button>
      </div>`;
    return;
  }

  const q = els.search.value.trim();
  const list = visibleArticles();
  if (list.length === 0) {
    els.items.innerHTML = "";
    els.empty.classList.add("visible");
    els.empty.textContent =
      activeFeedId === null
        ? "No feeds on this device yet — add from Discover below or paste a feed URL."
        : q
          ? "No signals match your search."
          : "No articles yet — this feed may not have been fetched yet.";
    return;
  }
  els.empty.classList.remove("visible");

  els.items.innerHTML = list
    .map((a) => {
      const source = feedById(a.feed_id)?.title || domainOf(a.link);
      const summary = escapeHtml((a.summary || "").slice(0, 220));
      return `<a class="item" href="${escapeHtml(a.link)}" target="_blank" rel="noopener" data-aos="fade-up">
        <div>
          <div class="source">${escapeHtml(source)}</div>
          <h2 class="item-title">${escapeHtml(a.title)}</h2>
          ${summary ? `<p class="item-summary">${summary}</p>` : ""}
          <div class="item-meta">
            <span class="tag">${escapeHtml(domainOf(a.link))}</span>
            <span class="tag time">${timeAgo(a.published_at)}</span>
          </div>
        </div>
        <div class="item-aside"><span class="time">↗</span></div>
      </a>`;
    })
    .join("");

  refreshMotion();
}

async function loadArticles(): Promise<void> {
  if (activeFeedId === null) {
    articles = [];
    articlesState = "loaded";
    renderItems();
    return;
  }
  articlesState = "loading";
  els.items.innerHTML = `<div class="skeleton"></div>`;
  els.empty.classList.remove("visible");
  try {
    articles = await getArticles(activeFeedId);
    articlesState = "loaded";
  } catch {
    articles = [];
    articlesState = "error";
  }
  renderItems();
}

/** Load THIS device's feed list (navigation-only — never the pool catalog).
 *  Failure renders an in-nav error + Retry — never a blank "no feeds": empty is
 *  only shown on a successful empty list (a brand-new device). */
async function loadFeeds(): Promise<void> {
  feedsState = "loading";
  renderNav();
  try {
    feeds = await getMyFeeds();
  } catch {
    feeds = [];
    activeFeedId = null;
    feedsState = "error";
    renderNav();
    return;
  }
  feedsState = "loaded";
  const keep =
    activeFeedId && feeds.some((f) => f.id === activeFeedId)
      ? activeFeedId
      : feeds[0]?.id ?? null;
  activeFeedId = keep;
  renderNav();
  renderDiscover();
  await loadArticles();
}

/** Load the shared-pool catalog into the Discover strip. Auxiliary — a pool
 *  failure hides Discover, it never touches the device nav. */
async function loadPool(): Promise<void> {
  try {
    poolFeeds = await getFeeds();
  } catch {
    poolFeeds = [];
  }
  renderDiscover();
}

/** Render the one-click subscribe strip: shared-pool feeds this device does not
 *  already follow. Never auto-subscribes — the strip is a suggestion only, and
 *  an empty list stays empty until the user acts. Hides itself when nothing is
 *  left to suggest. */
function renderDiscover(): void {
  const mine = new Set(feeds.map((f) => f.id));
  const suggestions = poolFeeds.filter((f) => !mine.has(f.id));
  if (suggestions.length === 0) {
    els.discover.hidden = true;
    els.discover.innerHTML = "";
    return;
  }
  els.discover.hidden = false;
  els.discover.innerHTML =
    `<div class="discover-label">Discover shared feeds</div>` +
    suggestions
      .map((f) => {
        const name = escapeHtml(f.title || domainOf(f.url));
        return `<button class="discover-feed" type="button" data-action="subscribe-discover"
                data-feed="${f.id}" title="Subscribe ${name} on this device">
          <span class="discover-plus" aria-hidden="true">+</span>
          <span class="discover-name">${name}</span>
        </button>`;
      })
      .join("");
}

/** Subscribe this device to a shared-pool feed from the Discover strip, then
 *  reload so the feed appears in the nav and drops out of Discover. */
async function subscribeDiscover(feedId: number): Promise<void> {
  const feed = poolFeeds.find((f) => f.id === feedId);
  const rawName = feed?.title || (feed ? domainOf(feed.url) : String(feedId));
  try {
    await subscribeFeed(feedId);
    await loadAll();
    toast(`Added ${rawName} to this device`);
  } catch (err) {
    toast(err instanceof ApiError ? err.message : String(err), true);
  }
}

/** Load diagnostics; auxiliary only — on failure the feeds still render. */
async function loadDiagnostics(): Promise<void> {
  try {
    diagnostics = await getDiagnostics();
    diagOk = true;
  } catch {
    diagnostics = null;
    diagOk = false;
  }
  renderStats();
}

async function loadAll(): Promise<void> {
  els.subtitle.textContent = "Loading…";
  els.refresh.disabled = true;
  try {
    // Independent: one failing endpoint must not blank the others.
    await Promise.all([loadFeeds(), loadPool(), loadDiagnostics()]);
    // Discover subtracts the device list from the pool — re-render now that
    // both have settled (whichever resolved last).
    renderDiscover();
  } finally {
    els.refresh.disabled = false;
  }
}

function selectFeed(id: number): void {
  activeFeedId = id;
  renderNav();
  void loadArticles();
}

/** Unsubscribe THIS device from a pool feed. The toast distinguishes a plain
 *  per-device unsubscribe from the case where this device was the LAST
 *  subscriber and the worker pruned the feed + its articles from the shared
 *  pool. The id may already be gone from state by the time this runs (async
 *  destructive action), so it exits quietly when the feed is no longer
 *  resolvable. */
async function removeFeed(id: number): Promise<void> {
  const feed = feedById(id);
  if (!feed) return;
  // Plain-text name for the dialog/toast — never HTML-escaped here.
  const rawName = feed.title || domainOf(feed.url);
  const ok = window.confirm(`Remove "${rawName}" from this device?`);
  if (!ok) return;
  try {
    const { pruned } = await deleteFeed(id);
    await loadAll();
    toast(
      pruned
        ? `Removed ${rawName} (last user — also removed from the shared pool)`
        : `Removed ${rawName} from this device`
    );
  } catch (err) {
    toast(err instanceof ApiError ? err.message : String(err), true);
  }
}

// --- Events -----------------------------------------------------------------
els.feedNav.addEventListener("click", (event) => {
  const action = (event.target as HTMLElement).closest<HTMLElement>("[data-action]");
  if (action?.dataset.action === "retry-feeds") {
    void loadFeeds();
    return;
  }
  // Remove must be handled before the [data-feed] select branch, otherwise
  // clicking the ✕ would also select the feed underneath it.
  if (action?.dataset.action === "remove-feed") {
    void removeFeed(Number(action.dataset.feed));
    return;
  }
  const btn = (event.target as HTMLElement).closest<HTMLElement>("[data-feed]");
  if (btn) selectFeed(Number(btn.dataset.feed));
});

// Discover strip lives outside #feedNav (its own container), so it gets its own
// delegated listener. Subscribe is additive only — nothing auto-subscribes.
els.discover.addEventListener("click", (event) => {
  const action = (event.target as HTMLElement).closest<HTMLElement>("[data-action]");
  if (action?.dataset.action === "subscribe-discover") {
    void subscribeDiscover(Number(action.dataset.feed));
  }
});

els.items.addEventListener("click", (event) => {
  const action = (event.target as HTMLElement).closest<HTMLElement>("[data-action]");
  if (action?.dataset.action === "retry-articles") void loadArticles();
});

els.search.addEventListener("input", renderItems);
els.sort.addEventListener("change", renderItems);

els.refresh.addEventListener("click", async () => {
  if (activeFeedId === null) {
    await loadAll();
    return;
  }
  try {
    els.refresh.textContent = "↻ Syncing…";
    await triggerFetch(activeFeedId);
    await loadAll();
    toast("Feed refreshed");
  } catch (err) {
    toast(err instanceof ApiError ? err.message : String(err), true);
  } finally {
    els.refresh.textContent = "↻ Refresh";
  }
});

els.feedForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const url = els.feedUrl.value.trim();
  const title = els.feedTitle.value.trim() || domainOf(url);
  if (!url) return;
  const rawName = title || domainOf(url);
  try {
    const { created, already } = await addFeed(url, title);
    els.feedUrl.value = "";
    els.feedTitle.value = "";
    await loadAll();
    // POST /api/feeds is idempotent and always succeeds; created/already tell
    // the honest story for the toast.
    toast(
      created
        ? `Added ${rawName} to this device`
        : already
          ? `${rawName} was already on this device`
          : `Added ${rawName} from the shared pool to this device`
    );
  } catch (err) {
    toast(err instanceof ApiError ? err.message : String(err), true);
  }
});

// --- Boot -------------------------------------------------------------------
initMotion();
void loadAll();

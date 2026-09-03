import {
  ApiError,
  addFeed,
  domainOf,
  getArticles,
  getDiagnostics,
  getFeeds,
  timeAgo,
  timeUntil,
  triggerFetch,
} from "../lib/api";
import type { Article, Diagnostics, Feed } from "../lib/types";
import { initMotion, refreshMotion } from "./animate";

/** App controller: owns UI state and rendering. Data access only via lib/api. */

function $<T extends HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`Missing #${id}`);
  return el as T;
}

const els = {
  feedNav: $<HTMLElement>("feedNav"),
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

let feeds: Feed[] = [];
let articles: Article[] = [];
let activeFeedId: number | null = null;
let diagnostics: Diagnostics | null = null;
let toastTimer = 0;

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

/** Derive the per-feed health line from the fields `/api/feeds` already sends. */
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
  const html = feeds
    .map((f) => {
      const title = escapeHtml(f.title || domainOf(f.url));
      const health = feedHealth(f);
      const dotCls = `dot ${health.tone}`;
      const metaErr = health.tone === "err" ? "err" : "";
      const active = f.id === activeFeedId ? "active" : "";
      const tip = `${health.badge} · ${health.detail}`;
      return `<button class="nav-item ${active}" type="button" data-feed="${f.id}" title="${escapeHtml(
        tip
      )}">
        <span class="nav-row">
          <span class="${dotCls}"></span><span class="nav-title">${title}</span>
        </span>
        <span class="nav-meta ${metaErr}">
          <span class="health-badge">${escapeHtml(health.badge)}</span>
          <span>${escapeHtml(health.detail)}</span>
        </span>
      </button>`;
    })
    .join("");
  els.feedNav.innerHTML = html || `<div class="nav-label">No feeds yet</div>`;
}

function renderStats(): void {
  if (!diagnostics) return;
  const byStatus = Object.fromEntries(
    diagnostics.feeds_by_status.map((s) => [s.status, s.c])
  );
  const total = diagnostics.articles_total[0]?.total ?? 0;
  const active = byStatus["active"] ?? 0;
  const errors = byStatus["error"] ?? 0;
  const run = diagnostics.last_fetch_run;
  const lastSync = run?.started_at ?? diagnostics.cron_ticks[0]?.last_tick ?? null;
  const runFailed = run?.feeds_failed ?? 0;

  els.healthDot.classList.toggle("error", errors > 0);
  els.healthText.textContent = `${active} active · ${errors} failed · sync ${timeAgo(
    lastSync
  )}${runFailed ? ` · ${runFailed} failed this run` : ""}`;
  els.subtitle.textContent = `${active} live feeds · ${total} articles stored`;

  const stat = (value: string | number, label: string): string =>
    `<div class="stat"><div class="value">${value}</div><div class="label">${label}</div></div>`;

  els.stats.innerHTML =
    stat(active, "Active feeds") +
    stat(errors, "Failed feeds") +
    stat(total, "Articles") +
    stat(timeAgo(lastSync), "Last sync");
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
  const list = visibleArticles();
  els.empty.classList.toggle("visible", list.length === 0);
  els.empty.textContent = "No signals match your search.";

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
    renderItems();
    return;
  }
  els.items.innerHTML = `<div class="skeleton"></div>`;
  articles = await getArticles(activeFeedId);
  renderItems();
}

async function loadAll(): Promise<void> {
  try {
    els.subtitle.textContent = "Loading…";
    els.refresh.disabled = true;

    const [feedList, diag] = await Promise.all([getFeeds(), getDiagnostics()]);
    feeds = feedList;
    diagnostics = diag;

    const keep =
      activeFeedId && feeds.some((f) => f.id === activeFeedId)
        ? activeFeedId
        : feeds[0]?.id ?? null;
    activeFeedId = keep;

    renderNav();
    renderStats();
    await loadArticles();
  } catch (err) {
    const message = err instanceof ApiError ? err.message : String(err);
    toast(message, true);
    els.subtitle.textContent = message;
  } finally {
    els.refresh.disabled = false;
  }
}

function selectFeed(id: number): void {
  activeFeedId = id;
  renderNav();
  void loadArticles();
}

// --- Events -----------------------------------------------------------------
els.feedNav.addEventListener("click", (event) => {
  const btn = (event.target as HTMLElement).closest<HTMLElement>("[data-feed]");
  if (btn) selectFeed(Number(btn.dataset.feed));
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
  try {
    await addFeed(url, title);
    els.feedUrl.value = "";
    els.feedTitle.value = "";
    await loadAll();
    toast("Feed added");
  } catch (err) {
    toast(err instanceof ApiError ? err.message : String(err), true);
  }
});

// --- Boot -------------------------------------------------------------------
initMotion();
void loadAll();

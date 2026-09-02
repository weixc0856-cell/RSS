import {
  ApiError,
  addFeed,
  domainOf,
  getArticles,
  getDiagnostics,
  getFeeds,
  timeAgo,
  triggerFetch,
} from "../lib/api";
import type { Article, Diagnostics, EnvKey, Feed } from "../lib/types";
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
  envDev: $<HTMLButtonElement>("envDev"),
  envProd: $<HTMLButtonElement>("envProd"),
  feedForm: $<HTMLFormElement>("feedForm"),
  feedUrl: $<HTMLInputElement>("feedUrl"),
  feedTitle: $<HTMLInputElement>("feedTitle"),
  toast: $<HTMLElement>("toast"),
};

let env: EnvKey = (localStorage.getItem("rss-env") as EnvKey) || "dev";
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

function renderEnvSwitch(): void {
  els.envDev.classList.toggle("active", env === "dev");
  els.envProd.classList.toggle("active", env === "prod");
}

function renderNav(): void {
  const html = feeds
    .map((f) => {
      const title = escapeHtml(f.title || domainOf(f.url));
      const cls = f.status === "error" ? "dot error" : "dot";
      const active = f.id === activeFeedId ? "active" : "";
      return `<button class="nav-item ${active}" type="button" data-feed="${f.id}">
        <span class="${cls}"></span><span>${title}</span>
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
  const tick = diagnostics.cron_ticks[0];
  const lastTick = tick?.last_tick ? ` · cron ${timeAgo(tick.last_tick)}` : "";

  els.healthDot.classList.toggle("error", errors > 0);
  els.healthText.textContent = `${env} · ${active} active · ${errors} errors${lastTick}`;
  els.subtitle.textContent = `${active} live feeds · ${total} articles stored`;

  const stat = (value: string | number, label: string): string =>
    `<div class="stat"><div class="value">${value}</div><div class="label">${label}</div></div>`;

  els.stats.innerHTML =
    stat(active, "Active feeds") +
    stat(errors, "Failed feeds") +
    stat(total, "Articles") +
    stat(env.toUpperCase(), "Environment");
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
  articles = await getArticles(env, activeFeedId);
  renderItems();
}

async function loadAll(): Promise<void> {
  try {
    els.subtitle.textContent = "Loading…";
    els.refresh.disabled = true;

    const [feedList, diag] = await Promise.all([
      getFeeds(env),
      getDiagnostics(env),
    ]);
    feeds = feedList;
    diagnostics = diag;

    const keep =
      activeFeedId && feeds.some((f) => f.id === activeFeedId)
        ? activeFeedId
        : feeds[0]?.id ?? null;
    activeFeedId = keep;

    renderNav();
    renderEnvSwitch();
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
    await triggerFetch(env, activeFeedId);
    await loadAll();
    toast("Feed refreshed");
  } catch (err) {
    toast(err instanceof ApiError ? err.message : String(err), true);
  } finally {
    els.refresh.textContent = "↻ Refresh";
  }
});

els.envDev.addEventListener("click", () => {
  env = "dev";
  localStorage.setItem("rss-env", env);
  void loadAll();
});
els.envProd.addEventListener("click", () => {
  env = "prod";
  localStorage.setItem("rss-env", env);
  void loadAll();
});

els.feedForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const url = els.feedUrl.value.trim();
  const title = els.feedTitle.value.trim() || domainOf(url);
  if (!url) return;
  try {
    await addFeed(env, url, title);
    els.feedUrl.value = "";
    els.feedTitle.value = "";
    await loadAll();
    toast("Feed added");
  } catch (err) {
    toast(err instanceof ApiError ? err.message : String(err), true);
  }
});

// --- Boot -------------------------------------------------------------------
renderEnvSwitch();
initMotion();
void loadAll();

#!/usr/bin/env node
/**
 * RSS Intelligence — read-only production article-contract checks.
 *
 * Asserts the `published_at` canonical invariant (see ARCHITECTURE.md §3.1) on
 * the live production Worker + D1. This is the data-plane acceptance for the
 * article-ordering fix: string sort == time sort only under the fixed 20-char
 * `YYYY-MM-DDTHH:MM:SSZ` form, and `ORDER BY published_at DESC` /
 * `MAX(published_at)` depend on it.
 *
 * Checks (all read-only — no writes, no fetches, no cron triggers):
 *   1. D1: every non-NULL `articles.published_at` matches the canonical
 *      skeleton; zero duplicate article hashes.
 *   2. Topology-agnostic feed accounting: the D1 `feeds` count equals the
 *      `/api/feeds` listing length, and `/api/health`'s enabled-feeds counts
 *      are internally consistent (active + failed == total, total within the
 *      full listing). No fixed feed count is asserted — the set legitimately
 *      grows/shrinks over time.
 *   3. `/api/health`: `newest_published_at` is canonical AND recent (< 48h —
 *      proves MAX is real time, not a stale lexicographic artifact).
 *   4. Every feed in `/api/feeds`: `/api/feeds/:id/articles` window has every
 *      `published_at` canonical and strictly DESC (monotonic); feeds whose
 *      status is `active` are additionally expected non-empty.
 *
 * Note: D1 rejects a strict 16-bracket `[0-9]` GLOB as "pattern too complex",
 * so the SQL sanity uses the fixed-width `?`-skeleton (D1 `?` = one char). The
 * authoritative strict-regex verification lives in the backfill script's
 * `remaining_noncanonical` (and in the Rust write-contract unit tests).
 *
 * Usage: node scripts/check-articles-contract.mjs [--base <url>]
 */
import { d1, loadEnv } from "./lib/cf-api.mjs";

const env = loadEnv();

const DB = process.env.PROD_D1_DATABASE_ID || env.PROD_D1_DATABASE_ID;
const baseIdx = process.argv.indexOf("--base");
const BASE =
  baseIdx !== -1 && process.argv[baseIdx + 1]
    ? process.argv[baseIdx + 1]
    : "https://rss-worker-production.weixc0856.workers.dev";
const CANON = /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$/;
if (!DB) {
  console.error("Missing PROD_D1_DATABASE_ID in .env files.");
  process.exit(2);
}

let fails = 0;
const ok = (name, cond, detail = "") => {
  console.log(`[${cond ? "PASS" : "FAIL"}] ${name}${cond ? "" : "  " + detail}`);
  if (!cond) fails++;
};

const [r] = await d1(
  DB,
  `SELECT
     (SELECT COUNT(*) FROM articles) total,
     (SELECT COUNT(*) FROM articles WHERE published_at IS NULL) nulls,
     (SELECT COUNT(*) FROM articles WHERE published_at IS NOT NULL AND published_at NOT GLOB '????-??-??T??:??:??Z') noncanon,
     (SELECT COUNT(*) FROM feeds) feeds,
     (SELECT COUNT(*) FROM (SELECT hash FROM articles GROUP BY hash HAVING COUNT(*) > 1)) dups`
);
ok("D1: articles non-NULL all canonical (skeleton)", r.nulls === 0 && r.noncanon === 0, `nulls=${r.nulls} noncanon=${r.noncanon}`);
ok("D1: no duplicate hashes", r.dups === 0, `dups=${r.dups}`);

const feedsRes = await fetch(BASE + "/api/feeds");
const feedsBody = await feedsRes.json();
const feeds = Array.isArray(feedsBody.data) ? feedsBody.data : [];
ok("/api/feeds returns an array", feedsRes.ok && Array.isArray(feedsBody.data), String(feedsBody).slice(0, 80));

// Topology-agnostic accounting: D1 and the two HTTP endpoints must agree on
// how many feeds exist. No absolute count is pinned.
ok("D1 feeds count == /api/feeds listing length", r.feeds === feeds.length, `d1=${r.feeds} http=${feeds.length}`);

const health = await (await fetch(BASE + "/api/health")).json();
const h = health.data;
if (h?.feeds) {
  ok(
    "health active + failed == total",
    h.feeds.active + h.feeds.failed === h.feeds.total,
    JSON.stringify(h.feeds)
  );
  ok(
    "health enabled feeds within full listing",
    h.feeds.total <= feeds.length,
    `health=${h.feeds.total} listing=${feeds.length}`
  );
} else {
  ok("health returns feeds counts", false, JSON.stringify(h).slice(0, 80));
}
if (h?.articles?.newest_published_at) {
  ok("health newest_published_at is canonical ISO", CANON.test(h.articles.newest_published_at), h.articles.newest_published_at);
  ok(
    "health newest_published_at is recent (<48h)",
    Date.now() - Date.parse(h.articles.newest_published_at) < 48 * 3600 * 1000,
    h.articles.newest_published_at
  );
}

for (const feed of feeds) {
  const id = feed.id;
  const res = await fetch(BASE + `/api/feeds/${id}/articles`);
  const body = await res.json();
  const arts = body.data?.articles ?? body.data;
  ok(`feed#${id}: /api returns 200 + array`, res.ok && Array.isArray(arts), String(body).slice(0, 80));
  if (!Array.isArray(arts)) continue;
  // An `active` feed has fetched successfully at least once, so it should
  // have persisted articles. `error`/`pending` feeds may legitimately be empty.
  ok(
    `feed#${id}: active feed window non-empty`,
    feed.status !== "active" || arts.length > 0,
    `status=${feed.status} len=${arts.length}`
  );
  const times = arts.map((a) => a.published_at);
  ok(`feed#${id}: every published_at canonical`, times.every((t) => t && CANON.test(t)), JSON.stringify(times.slice(0, 3)));
  ok(`feed#${id}: DESC order monotonic`, times.every((t, i) => i === 0 || times[i - 1] >= t));
}

console.log(fails ? `\nRESULT: ${fails} check(s) FAILED` : "\nRESULT: ALL ARTICLE-CONTRACT CHECKS PASSED");
process.exit(fails ? 1 : 0);

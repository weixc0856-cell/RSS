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
 *      skeleton; feeds == 3; zero duplicate article hashes.
 *   2. `/api/health`: feeds 3/active/0-failed; `newest_published_at` is
 *      canonical AND recent (< 48h — proves MAX is real time, not a stale
 *      lexicographic artifact).
 *   3. Per feed `/api/feeds/:id/articles`: non-empty 50-window, every
 *      `published_at` canonical, strictly DESC (monotonic).
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
ok("D1: feeds == 3", r.feeds === 3, `feeds=${r.feeds}`);
ok("D1: no duplicate hashes", r.dups === 0, `dups=${r.dups}`);

const health = await (await fetch(BASE + "/api/health")).json();
const h = health.data;
ok("health success + feeds 3/0/3", h.feeds.total === 3 && h.feeds.active === 3 && h.feeds.failed === 0, JSON.stringify(h.feeds));
ok("health newest_published_at is canonical ISO", CANON.test(h.articles.newest_published_at), h.articles.newest_published_at);
ok(
  "health newest_published_at is recent (<48h)",
  Date.now() - Date.parse(h.articles.newest_published_at) < 48 * 3600 * 1000,
  h.articles.newest_published_at
);

for (const id of [1, 2, 3]) {
  const res = await fetch(BASE + `/api/feeds/${id}/articles`);
  const body = await res.json();
  const arts = body.data?.articles ?? body.data;
  ok(`feed#${id}: /api returns 200 + array`, res.ok && Array.isArray(arts), String(body).slice(0, 80));
  if (!Array.isArray(arts)) continue;
  ok(`feed#${id}: window non-empty`, arts.length > 0, `len=${arts.length}`);
  const times = arts.map((a) => a.published_at);
  ok(`feed#${id}: every published_at canonical`, times.every((t) => t && CANON.test(t)), JSON.stringify(times.slice(0, 3)));
  ok(`feed#${id}: DESC order monotonic`, times.every((t, i) => i === 0 || times[i - 1] >= t));
}

console.log(fails ? `\nRESULT: ${fails} check(s) FAILED` : "\nRESULT: ALL ARTICLE-CONTRACT CHECKS PASSED");
process.exit(fails ? 1 : 0);

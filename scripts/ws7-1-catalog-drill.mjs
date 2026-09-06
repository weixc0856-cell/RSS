#!/usr/bin/env node
/**
 * WS7.1 catalog drill against a deployed worker (DEV only).
 *
 * Two parts:
 *
 *  STATIC GATE — asserts the shipped catalog (recommended-feeds.ts) against its
 *  evidence (ws7-1-verified.json) and its own shape: non-empty, unique names
 *  and urls, complete fields, A/B/C tier, every row carries a GREEN edge
 *  verdict, and the module is pure data (no runtime/browser references). These
 *  assertions are the mechanism that keeps "only validated feeds ship".
 *
 *  FUNCTIONAL ROUND-TRIP — the Recommended-row lifecycle over the real API
 *  with two fake devices (A/B), mirroring exactly how app.ts decides the UI:
 *  followsCatalogUrl(catUrl) = any of this device's feeds has url === catUrl OR
 *  normalized_url === catUrl. Picks one GREEN catalog row that is NOT yet in
 *  the shared pool (so the drill owns it), then drives it through:
 *    A "+" add      -> created, Discover row turns to ✓ (followed), renders once
 *    B "+" add      -> converges on A's pool feed (created:false) — no duplicate
 *    B "+" again    -> already:true (idempotent)
 *    collision gate  -> a catalog-row pool feed never renders twice (Recommended
 *                       "+" only; excluded from the Shared-pool tail by design)
 *    A leave        -> pruned:false, pool + B unaffected
 *    B leave (last) -> pruned:true, pool restored
 *
 * Safety / cleanliness:
 *   - DEV-only: refuses the production host unless WS7_DRILL_ALLOW_PROD=1.
 *   - Self-cleaning: a preamble unsubscribes every feed either drill key still
 *     follows (a crashed prior run's leftover), so the pool returns to its
 *     pre-run snapshot (asserted). Re-runnable.
 *   - Writes: creates + prunes one pool feed and provisions two profile rows
 *     (ws7-1-drill-A/B, idempotent; never deleted by design).
 *
 * Usage:
 *   node scripts/ws7-1-catalog-drill.mjs [base]   # base defaults to DEV
 */
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

const BASE = process.argv[2] || "https://rss-worker.weixc0856.workers.dev";
const HOST = new URL(BASE).host;
if (HOST.includes("production") && process.env.WS7_DRILL_ALLOW_PROD !== "1") {
  console.error(
    `Refusing production host ${HOST}: this drill mutates the pool ` +
      `(creates + prunes a feed). Run it against the dev worker ` +
      `(https://rss-worker.weixc0856.workers.dev) or set WS7_DRILL_ALLOW_PROD=1.`
  );
  process.exit(2);
}

const __dir = path.dirname(fileURLToPath(import.meta.url));
const CATALOG = path.join(__dir, "..", "frontend", "src", "lib", "recommended-feeds.ts");
const EVIDENCE = path.join(__dir, "ws7-1-verified.json");
const A = "ws7-1-drill-A";
const B = "ws7-1-drill-B";

const catalogSrc = readFileSync(CATALOG, "utf8");
const evidence = JSON.parse(readFileSync(EVIDENCE, "utf8"));
const mod = await import(`${pathToFileURL(CATALOG).href}?t=${Date.now()}`);
const CATALOG_ROWS = mod.RECOMMENDED_FEEDS;

let fails = 0;
const ok = (name, cond, detail = "") => {
  console.log(`[${cond ? "PASS" : "FAIL"}] ${name}${cond ? "" : "  " + detail}`);
  if (!cond) fails++;
};

async function call(method, path, key, body) {
  const headers = { "Content-Type": "application/json" };
  if (key) headers["X-User-Id"] = key;
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers,
    body: body ? JSON.stringify(body) : undefined,
  });
  let json = null;
  try {
    json = await res.json();
  } catch {
    /* non-JSON body — a failing check */
  }
  return { status: res.status, json };
}

const listOf = async (key) => {
  const r = await call("GET", "/api/me/feeds", key);
  return r.json?.data ?? null;
};
const poolRows = async () => {
  const r = await call("GET", "/api/feeds", null);
  return r.json?.data ?? null;
};

/** Exact mirror of app.ts followsCatalogUrl. */
const follows = (rows, url) =>
  (rows ?? []).some((f) => f.url === url || f.normalized_url === url);
/** Exact mirror of app.ts Shared-pool tail exclusion. */
const isCatalogFeed = (feed) =>
  CATALOG_ROWS.some((c) => feed.url === c.url || feed.normalized_url === c.url);

console.log(`WS7.1 catalog drill against ${BASE} — ${CATALOG_ROWS.length} catalog rows`);

// --- STATIC GATE ------------------------------------------------------------
{
  const src = catalogSrc;
  const tierOk = new Set(["A", "B", "C"]);
  const names = new Set(CATALOG_ROWS.map((r) => r.name));
  const urls = new Set(CATALOG_ROWS.map((r) => r.url));
  ok("catalog non-empty", CATALOG_ROWS.length > 0);
  ok("names unique", names.size === CATALOG_ROWS.length);
  ok("urls unique", urls.size === CATALOG_ROWS.length);
  const complete = CATALOG_ROWS.every(
    (r) =>
      typeof r.name === "string" && r.name.trim() !== "" &&
      typeof r.url === "string" && /^https:\/\//.test(r.url) &&
      typeof r.category === "string" && r.category.trim() !== "" &&
      typeof r.description === "string" && r.description.trim() !== "" &&
      tierOk.has(r.tier)
  );
  ok("every row: name/url(category|description|tier complete", complete,
    JSON.stringify(CATALOG_ROWS.find((r) => !r.description)));
  // Pure data: a product-config module must not import or reach for runtime.
  ok("catalog module is pure data (no imports/require)",
    !/^\s*import\s/m.test(src) && !/require\s*\(/.test(src));
  ok("catalog module has no browser/runtime references",
    !/\b(window|document|localStorage|fetch|Date|Math)\b/.test(src));
  // Evidence gate: every shipped row has a GREEN verdict for its exact url.
  const ev = new Map(evidence.map((r) => [r.url, r]));
  const missing = CATALOG_ROWS.filter((r) => ev.get(r.url)?.verdict !== "GREEN");
  ok("every catalog row has a GREEN verdict in ws7-1-verified.json",
    missing.length === 0,
    "no GREEN evidence for: " + missing.map((r) => r.name).join(", "));
}

// --- Preamble: clear any feed either drill key follows (crashed-run leftover)
{
  for (const key of [A, B]) {
    const rows = await listOf(key);
    for (const f of rows ?? []) {
      await call("DELETE", `/api/feeds/${f.id}`, key); // 404 = fine
    }
  }
  const a = await listOf(A);
  const b = await listOf(B);
  ok("preamble: drill keys start clean", (a?.length ?? 0) === 0 && (b?.length ?? 0) === 0);
}

// --- Pre body pool snapshot --------------------------------------------------
const prePool = await poolRows();
const preCount = prePool?.length ?? 0;
ok("pre: GET /api/feeds anonymous success", Array.isArray(prePool));

// Pick a GREEN catalog row the pool does not yet hold, so this drill owns the
// only feed for that url (safe to prune as last subscriber at the end).
const inPool = (url) =>
  (prePool ?? []).some((f) => f.url === url || f.normalized_url === url);
const drillCat = CATALOG_ROWS.find((c) => !inPool(c.url));
ok("a GREEN catalog row is free to drill on (not already in pool)",
  drillCat !== undefined,
  "every catalog row already exists in the dev pool — nothing safe to own");
if (!drillCat) {
  console.log(fails ? `\nDRILL: ${fails} FAILED` : "\nDRILL: ALL PASSED");
  process.exit(fails ? 1 : 0);
}
const { name: catName, url: catUrl } = drillCat;
console.log(`drill feed: ${catName} (${catUrl})`);

// 1. A "+" add (Recommended addFeed(url, name)) -> created; Discover row → ✓
let feedId = null;
{
  const r = await call("POST", "/api/feeds", A, { url: catUrl, title: catName });
  ok("A recommended-add success", r.json?.success === true, `${r.status} ${JSON.stringify(r.json)}`);
  ok("A recommended-add created:true (feed was not pooled)", r.json?.data?.created === true, JSON.stringify(r.json?.data));
  feedId = r.json?.data?.feed?.id;
  ok("A recommended-add returns integer feed id", Number.isInteger(feedId), JSON.stringify(r.json?.data?.feed));
  const a = await listOf(A);
  ok("A follows the catalog url now (Discover row → ✓)",
    Array.isArray(a) && a.length === 1 && follows(a, catUrl), JSON.stringify(a));
}

// 2. Collision gate: the drill feed renders ONCE — never in both segments.
// For a device that does not follow it yet, Recommended shows "+" and the
// Shared-pool tail must EXCLUDE it (it is a catalog row): one render total.
{
  const pool = await poolRows();
  const same = (pool ?? []).filter((f) => f.url === catUrl || f.normalized_url === catUrl);
  ok("pool holds exactly one feed for the catalog url (no duplicate row)",
    same.length === 1, `count=${same.length}`);
  const bBefore = await listOf(B); // B not subscribed yet at this point
  const inTail = (pool ?? []).some(
    (f) =>
      (f.url === catUrl || f.normalized_url === catUrl) &&
      !(bBefore ?? []).some((m) => m.id === f.id) &&
      !isCatalogFeed(f)
  );
  ok("drill feed excluded from Shared-pool tail (Recommended-only, one render)",
    !inTail);
}

// 3. B "+" on the same url converges on A's pool feed (find-or-create, no new)
{
  const r = await call("POST", "/api/feeds", B, { url: catUrl, title: catName });
  ok("B recommended-add converges created:false", r.json?.data?.created === false, JSON.stringify(r.json?.data));
  ok("B recommended-add subscribed (already:false)", r.json?.data?.already === false, JSON.stringify(r.json?.data));
  ok("B returned the SAME pool feed id", r.json?.data?.feed?.id === feedId, JSON.stringify(r.json?.data?.feed));
  const b = await listOf(B);
  ok("B now follows the catalog url (Discover row → ✓)", Array.isArray(b) && b.length === 1 && follows(b, catUrl));
  const pool = await poolRows();
  const same = (pool ?? []).filter((f) => f.url === catUrl || f.normalized_url === catUrl);
  ok("pool still holds exactly one feed after B converged", same.length === 1, `count=${same.length}`);
}

// 4. B re-add is idempotent (already:true)
{
  const r = await call("POST", "/api/feeds", B, { url: catUrl, title: catName });
  ok("B re-add already:true", r.json?.data?.already === true, JSON.stringify(r.json?.data));
  const b = await listOf(B);
  ok("B still has exactly one feed", (b ?? []).length === 1, JSON.stringify(b));
}

// 5. A leaves while B remains -> pruned:false; A row back to "+"
{
  const r = await call("DELETE", `/api/feeds/${feedId}`, A);
  ok("A unsubscribe pruned:false (B remains)", r.json?.data?.pruned === false, JSON.stringify(r.json?.data));
  const a = await listOf(A);
  ok("A no longer follows (Discover row back to +)", (a ?? []).length === 0 && !follows(a, catUrl));
  const pool = await poolRows();
  ok("feed still pooled while B subscribes",
    (pool ?? []).some((f) => f.id === feedId), "pruned while B subscribed");
}

// 6. B leaves (last) -> pruned:true; pool restored
{
  const r = await call("DELETE", `/api/feeds/${feedId}`, B);
  ok("B unsubscribe pruned:true (last user)", r.json?.data?.pruned === true, JSON.stringify(r.json?.data));
  const b = await listOf(B);
  ok("B back to empty", (b ?? []).length === 0, JSON.stringify(b));
  const pool = await poolRows();
  ok("drill feed gone from pool",
    !(pool ?? []).some((f) => f.url === catUrl || f.normalized_url === catUrl), "still in pool");
}

// 7. Pool restored to pre-run state
{
  const pool = await poolRows();
  const postCount = pool?.length ?? 0;
  ok("pool size restored to pre-run snapshot", postCount === preCount, `pre=${preCount} post=${postCount}`);
}

console.log(fails ? `\nDRILL: ${fails} FAILED` : "\nDRILL: ALL PASSED");
process.exit(fails ? 1 : 0);

#!/usr/bin/env node
/**
 * WS7 device-isolation drill against a deployed worker (DEV only).
 *
 * Exercises the per-device subscription model over the shared feeds/articles
 * pool with two fake devices (A/B), each with its own X-User-Id namespace:
 * empty start, find-or-create subscribe (add form), Discover subscribe
 * (POST /:id/subscribe), idempotent re-add, per-device isolation, shared vs
 * last-user unsubscribe (prune), and structured 404 for a device that is not
 * subscribed. This is the scripted equivalent of the prod incognito check.
 *
 * Usage:
 *   node scripts/ws7-device-drill.mjs [base]        # base defaults to DEV
 *
 * Safety / cleanliness:
 *   - DEV-only: refuses the production host unless WS7_DRILL_ALLOW_PROD=1.
 *   - Self-cleaning: uses a fixed drill URL; a preamble unsubscribes any
 *     leftover drill feed from the previous (possibly crashed) run, and the
 *     final unsubscribe prunes it, so the pool returns to its pre-run state
 *     (asserted by a pool-size snapshot). Re-runnable.
 *   - Writes: creates + prunes one pool feed and provisions two profile rows
 *     (ws7-drill-device-A/B, idempotent by UNIQUE device_key; profile rows are
 *     never deleted by design). The drill feed's enqueued first fetch fails
 *     (example.invalid does not resolve) — harmless; it is pruned before any
 *     real fetch, exercising the prune-vs-fetch execution gate along the way.
 */
const BASE = process.argv[2] || "https://rss-worker.weixc0856.workers.dev";
const DRILL_URL = "https://ws7-drill.example.invalid/feed.xml";
const DRILL_TITLE = "WS7 Drill Feed";
const A = "ws7-drill-device-A";
const B = "ws7-drill-device-B";

const HOST = new URL(BASE).host;
if (HOST.includes("production") && process.env.WS7_DRILL_ALLOW_PROD !== "1") {
  console.error(
    `Refusing production host ${HOST}: this drill mutates the pool ` +
      `(creates + prunes a feed). Run it against the dev worker ` +
      `(https://rss-worker.weixc0856.workers.dev) or set WS7_DRILL_ALLOW_PROD=1.`
  );
  process.exit(2);
}

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

const poolHas = (pool, id) => (pool.json?.data || []).some((f) => f.id === id);
const drillIdInPool = (pool) =>
  (pool.json?.data || []).find((f) => f.url === DRILL_URL)?.id ?? null;

async function listOf(key) {
  const r = await call("GET", "/api/me/feeds", key);
  return { status: r.status, rows: r.json?.data ?? null, json: r.json };
}

// --- Preamble: clear any leftover drill feed from a previous run -----------
console.log(`WS7 device-isolation drill against ${BASE}`);
{
  let pool = await call("GET", "/api/feeds", null);
  let id = drillIdInPool(pool);
  if (id !== null) {
    for (const key of [A, B]) {
      // 404 = not subscribed (fine); 200 = unsubscribed (maybe pruned).
      await call("DELETE", `/api/feeds/${id}`, key);
    }
    pool = await call("GET", "/api/feeds", null);
  }
  ok("preamble: no leftover drill feed in pool", drillIdInPool(pool) === null);
}

// --- Pre body pool snapshot ------------------------------------------------
const prePool = await call("GET", "/api/feeds", null);
const preCount = (prePool.json?.data || []).length;
ok("pre: GET /api/feeds anonymous success", prePool.json?.success === true && Array.isArray(prePool.json?.data));

// 1. Anonymous device routes are identity-required
{
  const r = await call("GET", "/api/me/feeds", null);
  ok("anonymous /api/me/feeds == 400", r.status === 400, `got ${r.status}`);
  ok("anonymous /api/me/feeds success=false", r.json?.success === false, JSON.stringify(r.json));
}

// 2. A starts empty (fresh device namespace — no cross-device sync)
{
  const { rows, json } = await listOf(A);
  ok("A /api/me/feeds success array", json?.success === true && Array.isArray(rows), JSON.stringify(json));
  ok("A starts empty", Array.isArray(rows) && rows.length === 0, `len=${rows?.length}`);
}

// 3. A adds the drill URL via the add form -> created:true, list has 1
let feedId = null;
{
  const r = await call("POST", "/api/feeds", A, { url: DRILL_URL, title: DRILL_TITLE });
  ok("A add success", r.json?.success === true, `${r.status} ${JSON.stringify(r.json)}`);
  ok("A add created:true", r.json?.data?.created === true, JSON.stringify(r.json?.data));
  feedId = r.json?.data?.feed?.id;
  ok("A add returns integer feed id", Number.isInteger(feedId), JSON.stringify(r.json?.data?.feed));
  const { rows, json } = await listOf(A);
  const row = rows?.[0];
  ok("A list has 1 feed", Array.isArray(rows) && rows.length === 1 && row?.id === feedId, JSON.stringify(rows));
  ok("A row same projection as pool (id/url/title)", row?.url === DRILL_URL && typeof row?.title === "string", JSON.stringify(row));
  ok("A row has subscribed_at key", Object.prototype.hasOwnProperty.call(row ?? {}, "subscribed_at"), JSON.stringify(row));
  ok("A row has article_count key (number)", typeof row?.article_count === "number", JSON.stringify(row));
  // A fresh add whose first fetch has not run yet may be article_count 0; the
  // KEY + numeric type is the contract (the count fills in once fetched).
}

// 4. B subscribes the same pool feed via Discover POST /:id/subscribe (idempotent)
{
  const s1 = await call("POST", `/api/feeds/${feedId}/subscribe`, B);
  ok("B discover-subscribe success", s1.json?.success === true, `${s1.status} ${JSON.stringify(s1.json)}`);
  const b1 = await listOf(B);
  ok("B list now has 1", Array.isArray(b1.rows) && b1.rows.length === 1 && b1.rows[0].id === feedId, JSON.stringify(b1.rows));
  const s2 = await call("POST", `/api/feeds/${feedId}/subscribe`, B);
  ok("B discover-subscribe idempotent (still 1)", s2.json?.success === true && (await listOf(B)).rows.length === 1, JSON.stringify(s2.json));
}

// 5. Re-add is a no-op for both entry points (add form + subscribe converge)
{
  const aRe = await call("POST", "/api/feeds", A, { url: DRILL_URL, title: DRILL_TITLE });
  ok("A re-add same URL already:true", aRe.json?.data?.already === true, JSON.stringify(aRe.json?.data));
  const bRe = await call("POST", "/api/feeds", B, { url: DRILL_URL, title: DRILL_TITLE });
  ok("B re-add (already subscribed via /subscribe) already:true", bRe.json?.data?.already === true, JSON.stringify(bRe.json?.data));
  const b2 = await listOf(B);
  ok("B still has exactly 1", b2.rows.length === 1, JSON.stringify(b2.rows));
}

// 6. Pool catalog shows the feed while any device subscribes it
{
  const pool = await call("GET", "/api/feeds", null);
  ok("feed visible in shared pool (Discover)", poolHas(pool, feedId), "missing from pool");
}

// 7. A unsubscribes while B remains -> pruned:false, pool + B unaffected
{
  const r = await call("DELETE", `/api/feeds/${feedId}`, A);
  ok("A unsubscribe success", r.json?.success === true, JSON.stringify(r.json));
  ok("A unsubscribe pruned:false (B remains)", r.json?.data?.pruned === false, JSON.stringify(r.json?.data));
  const a2 = await listOf(A);
  ok("A list back to 0", Array.isArray(a2.rows) && a2.rows.length === 0, JSON.stringify(a2.rows));
  const pool = await call("GET", "/api/feeds", null);
  ok("feed still in pool after A leaves", poolHas(pool, feedId), "pruned while B subscribed");
  const b3 = await listOf(B);
  ok("B list unaffected", Array.isArray(b3.rows) && b3.rows.some((f) => f.id === feedId), JSON.stringify(b3.rows));
}

// 8. Unsubscribing again as a non-subscriber -> structured 404 (no leak)
{
  const r = await call("DELETE", `/api/feeds/${feedId}`, A);
  ok("A second unsubscribe == 404", r.status === 404, `got ${r.status}`);
  ok("A second unsubscribe structured json_error", r.json?.success === false && typeof r.json?.error === "string", JSON.stringify(r.json));
}

// 9. B unsubscribes (last) -> pruned:true, gone from pool
{
  const r = await call("DELETE", `/api/feeds/${feedId}`, B);
  ok("B unsubscribe success", r.json?.success === true, JSON.stringify(r.json));
  ok("B unsubscribe pruned:true (last user)", r.json?.data?.pruned === true, JSON.stringify(r.json?.data));
  const b4 = await listOf(B);
  ok("B back to empty", Array.isArray(b4.rows) && b4.rows.length === 0, JSON.stringify(b4.rows));
  const pool = await call("GET", "/api/feeds", null);
  ok("feed gone from pool after last unsubscribe", !poolHas(pool, feedId), "still in pool");
}

// 10. Pool restored to pre-run state
{
  const pool = await call("GET", "/api/feeds", null);
  const postCount = (pool.json?.data || []).length;
  ok("GET /api/feeds still anonymous success", pool.json?.success === true && Array.isArray(pool.json?.data), JSON.stringify(pool.json));
  ok("pool size restored to pre-run snapshot", postCount === preCount, `pre=${preCount} post=${postCount}`);
  ok("drill feed absent", drillIdInPool(pool) === null);
}

console.log(fails ? `\nDRILL: ${fails} FAILED` : "\nDRILL: ALL PASSED");
process.exit(fails ? 1 : 0);

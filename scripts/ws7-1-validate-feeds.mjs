#!/usr/bin/env node
/**
 * WS7.1 edge feed-validation harness (DEV only) — maintenance / verification
 * tool, NOT part of the daily CI/build (it mutates the dev pool transiently).
 *
 * Reads the product catalog (frontend/src/lib/recommended-feeds.ts) and asks
 * the DEV worker's real fetch path (true Rust parser + Cloudflare-edge
 * reachability) to judge each candidate URL GREEN / AMBER / RED. Writes the
 * verdicts as evidence into scripts/ws7-1-verified.json. It never judges from
 * this box's network (China-local reachability is noise; V2EX returns 403 to a
 * bare curl yet fetches fine from the edge).
 *
 * Usage:
 *   node scripts/ws7-1-validate-feeds.mjs [base] [--url=<u>] [--force]
 *     base      dev worker URL (defaults to the dev worker); refuses the
 *               production host unless WS7_DRILL_ALLOW_PROD=1.
 *     --url=    validate only the catalog row whose url === <u>.
 *     --force   ignore the verified.json cache (re-validate edge rows); rows
 *               with evidence_source "prod-live" are never re-validated.
 *
 * Verdicts (mechanical — tier is editorial, never derived here):
 *   GREEN = HTTP ok + parse ok + >=1 article + every article has a non-empty
 *           title AND a valid link-or-guid + unique identity >= 1 + every
 *           non-null published_at matches the canonical 20-char ...Z shape.
 *   AMBER = HTTP/parse ok but 0 articles (upstream transiently empty).
 *   RED   = HTTP/parse failure OR any article-contract failure (incl. a
 *           non-canonical published_at).
 * Only GREEN rows may enter the shipped catalog (static Gate B asserts it).
 *
 * Cleanliness: runs as device key `ws7-1-validator`; a feed already present in
 * the dev pool is never touched; feeds this run creates are pruned afterwards
 * (DELETE as last subscriber). Re-runnable.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

// Positional base = first script arg that isn't a flag. process.argv still
// starts with the node binary + script path on Windows, so scan slice(2) only.
const BASE = (process.argv.slice(2).find((a) => !a.startsWith("-")) ??
  "https://rss-worker.weixc0856.workers.dev").trim();
const URL_ONLY = process.argv.find((a) => a.startsWith("--url="))?.slice(6);
const FORCE = process.argv.includes("--force");
const HOST = new URL(BASE).host;
if (HOST.includes("production") && process.env.WS7_DRILL_ALLOW_PROD !== "1") {
  console.error(`Refusing production host ${HOST}: this harness mutates the dev pool.`);
  process.exit(2);
}

const __dir = path.dirname(fileURLToPath(import.meta.url));
const CATALOG = path.join(__dir, "..", "frontend", "src", "lib", "recommended-feeds.ts");
const EVIDENCE = path.join(__dir, "ws7-1-verified.json");
const KEY = "ws7-1-validator";
const CANON = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function errCode(err) {
  return err?.cause?.code ?? err?.message ?? String(err);
}

// The CF-edge path from a China-local box drops TLS connections transiently
// (ECONNRESET). Retry each request a few times before failing the candidate.
async function call(method, url, key, body) {
  const headers = { "Content-Type": "application/json" };
  if (key) headers["X-User-Id"] = key;
  let lastErr;
  for (let attempt = 1; attempt <= 3; attempt++) {
    try {
      const res = await fetch(`${BASE}${url}`, {
        method, headers, body: body ? JSON.stringify(body) : undefined,
      });
      let json = null;
      try { json = await res.json(); } catch { /* non-JSON */ }
      return { status: res.status, json };
    } catch (err) {
      lastErr = err;
      if (attempt < 3) {
        const delay = 700 * attempt;
        console.log(`[retry] ${method} ${url} — ${errCode(err)}; retrying in ${delay}ms`);
        await sleep(delay);
      }
    }
  }
  throw lastErr;
}

// Import the catalog .ts (frontend is "type":"module"; Node >=23.6 strips types).
const mod = await import(`${pathToFileURL(CATALOG).href}?t=${Date.now()}`);
const catalog = mod.RECOMMENDED_FEEDS;

const evidence = JSON.parse(readFileSync(EVIDENCE, "utf8"));
const byUrl = new Map(evidence.map((r) => [r.url, r]));

console.log(`WS7.1 feed-validation against ${BASE} — ${catalog.length} catalog rows`);

// Pre-run dev pool (url + normalized_url) so we never touch pre-existing feeds.
const pool = (await call("GET", "/api/feeds")).json?.data ?? [];
const poolUrl = new Set();
for (const f of pool) {
  if (f.url) poolUrl.add(f.url);
  if (f.normalized_url) poolUrl.add(f.normalized_url);
}

async function fetchOutcome(feedId) {
  // Poll the feed row until the first fetch lands or fails (initial fetch is
  // enqueued on add). Then one manual retry window via the anonymous
  // POST /:id/fetch (execution gate passes while the validator is subscribed).
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    const list = (await call("GET", "/api/feeds")).json?.data ?? [];
    const row = list.find((f) => f.id === feedId);
    if (!row) return { row: null, error: "feed row vanished" };
    if (row.last_fetched_at || row.error_message) return { row, error: null };
    await sleep(4000);
  }
  await call("POST", `/api/feeds/${feedId}/fetch`); // anonymous, gated
  const deadline2 = Date.now() + 45_000;
  while (Date.now() < deadline2) {
    const list = (await call("GET", "/api/feeds")).json?.data ?? [];
    const row = list.find((f) => f.id === feedId);
    if (!row) return { row: null, error: "feed row vanished" };
    if (row.last_fetched_at || row.error_message) return { row, error: null };
    await sleep(4000);
  }
  const list = (await call("GET", "/api/feeds")).json?.data ?? [];
  return { row: list.find((f) => f.id === feedId) ?? null, error: "no fetch after retry" };
}

function judge(row, articles) {
  if (row?.error_message) {
    return { verdict: "RED", error: `fetch error: ${row.error_message}` };
  }
  if (row && row.last_http_status && row.last_http_status >= 400) {
    return { verdict: "RED", error: `http ${row.last_http_status}` };
  }
  if (!row?.last_fetched_at && !row?.error_message) {
    return { verdict: "RED", error: "never fetched" };
  }
  if (!articles || articles.length === 0) {
    return { verdict: "AMBER", error: "http/parse ok but 0 articles" };
  }
  const badTitle = articles.filter((a) => !a.title || !String(a.title).trim()).length;
  const noId = articles.filter((a) => a.id === null || a.id === undefined).length;
  const noLink = articles.filter(
    (a) => !a.link && !a.guid).length;
  const nonCanon = articles.filter(
    (a) => a.published_at != null && !CANON.test(String(a.published_at))).length;
  if (badTitle > 0 || noId > 0 || noLink > 0 || nonCanon > 0) {
    return {
      verdict: "RED",
      error: `article contract: empty-title=${badTitle} no-id=${noId} no-link/guid=${noLink} non-canonical-pub=${nonCanon}`,
    };
  }
  return { verdict: "GREEN", error: null };
}

// Validate one candidate end-to-end against the dev worker. Every path —
// including an unexpected throw — still attempts the cleanup DELETE (finally),
// so a transient network failure can never leave a stray feed in the pool.
async function runCandidate(f) {
  const rec = {
    name: f.name, url: f.url, category: f.category,
    http_status: null, format: null, parsed_count: null, first_pub: null,
    sample_title: null, canonical_ok: null,
    error: null, verdict: "RED", tier: f.tier,
    evidence_source: "edge", evidence_date: new Date().toISOString().slice(0, 10),
  };
  let feedId = null;
  try {
    // Add -> subscribe validator -> initial fetch is enqueued.
    const add = await call("POST", "/api/feeds", KEY, { url: f.url, title: f.name });
    const feed = add.json?.data?.feed;
    if (!feed?.id) {
      console.log(`[FAIL] ${f.name} add rejected: ${JSON.stringify(add.json)}`);
      return { ...rec, error: `add rejected ${add.status}`, evidence_source: "note" };
    }
    feedId = feed.id;

    const { row, error } = await fetchOutcome(feed.id);
    let articles = [];
    if (row?.id) {
      const art = await call("GET", `/api/feeds/${row.id}/articles`);
      articles = art.json?.data ?? [];
    }
    const judgeResult = error
      ? { verdict: "RED", error }
      : judge(row, articles);
    const canonicalOk = articles.every(
      (a) => a.published_at == null || CANON.test(String(a.published_at)));
    return {
      ...rec,
      http_status: row?.last_http_status ?? null,
      parsed_count: articles.length,
      first_pub: articles[0]?.published_at ?? null,
      sample_title: articles[0]?.title ?? null,
      canonical_ok: articles.length ? canonicalOk : null,
      error: judgeResult.error,
      verdict: judgeResult.verdict,
    };
  } catch (err) {
    return { ...rec, error: `unhandled: ${errCode(err)}`, verdict: "RED" };
  } finally {
    if (feedId != null) {
      try {
        await call("DELETE", `/api/feeds/${feedId}`, KEY);
      } catch (err) {
        console.log(`[warn] cleanup DELETE /api/feeds/${feedId} failed: ${errCode(err)}`);
      }
    }
  }
}

const results = [];
for (const f of catalog) {
  const prev = byUrl.get(f.url);
  if (URL_ONLY && f.url !== URL_ONLY) { results.push(prev ?? null); continue; }
  if (prev?.verdict === "GREEN" && prev.evidence_source === "prod-live" && !URL_ONLY) {
    console.log(`[SKIP] ${f.name} (prod-live anchor)`);
    results.push(prev);
    continue;
  }
  if (prev?.verdict === "GREEN" && !FORCE && !URL_ONLY) {
    console.log(`[SKIP] ${f.name} (cached GREEN ${prev.evidence_date})`);
    results.push(prev);
    continue;
  }
  if (poolUrl.has(f.url)) {
    console.log(`[SKIP] ${f.name} (already in dev pool — not touched)`);
    if (prev) { results.push(prev); continue; }
    results.push({
      name: f.name, url: f.url, category: f.category,
      http_status: null, format: null, parsed_count: null, first_pub: null,
      sample_title: null, canonical_ok: null, error: "already in dev pool; not revalidated",
      verdict: "RED", tier: f.tier, evidence_source: "note", evidence_date: new Date().toISOString().slice(0, 10),
    });
    continue;
  }

  const rec = await runCandidate(f);
  console.log(`[${rec.verdict}] ${f.name} — ${rec.parsed_count ?? "?"} articles, http=${rec.http_status ?? "?"}${rec.error ? " " + rec.error : ""}`);
  results.push(rec);
}

// Merge: keep prior rows not re-validated this run, replace validated ones.
const out = evidence.filter((r) => !results.some((n) => n && n.url === r.url));
for (const r of results) if (r) out.push(r);
writeFileSync(EVIDENCE, JSON.stringify(out, null, 2) + "\n");

const n = { GREEN: 0, AMBER: 0, RED: 0 };
for (const r of out) if (r.verdict) n[r.verdict]++;
console.log(`\nWROTE ${out.length} evidence rows to ws7-1-verified.json (${n.GREEN} GREEN / ${n.AMBER} AMBER / ${n.RED} RED)`);
console.log(`\n${n.RED > 0 ? "some rows need review" : "ALL VALIDATED"}`);
process.exit(0);

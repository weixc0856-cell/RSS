#!/usr/bin/env node
/**
 * RSS Intelligence — one-time production history backfill: rewrite
 * articles.published_at to the canonical, chronologically sortable UTC ISO
 * form `YYYY-MM-DDTHH:MM:SSZ` (fixed 20-char shape), matching what the Rust
 * writer now stores via `src/utils.rs::normalize_published_at`.
 *
 * Why: RSS `<pubDate>` used to be stored verbatim (RFC822 text like
 * `Wed, 02 Sep 2026 12:00:00 GMT`). String sort — which `ORDER BY
 * published_at DESC` and `MAX(published_at)` rely on — equals chronological
 * sort ONLY under the single fixed canonical form. This script migrates the
 * pre-existing rows so history matches the new writer.
 *
 * ORDERING INVARIANT: run this AFTER the Worker (which contains the
 * normalizer) is deployed — otherwise a still-old Worker refetches feeds and
 * writes RFC822 text back in behind the backfill.
 *
 * Semantics mirror the Rust normalizer:
 *   - already canonical                        -> already_normalized (skip)
 *   - parses (RFC3339/ISO or RFC822 with the   -> will_normalize (UPDATE)
 *     trailing GMT/UT/UTC zone rewritten to +0000, then Date.parse)
 *   - otherwise                                -> unparseable (skip, keep raw
 *                                                 verbatim — no data loss)
 * Rust (chrono) and V8 (Date.parse) are different parsers. The `--dry-run`
 * run is therefore the record of what a real run will change: it prints the
 * full pre-statistics AND locks a self-check table whose expected values are
 * exactly the strings the Rust unit tests assert. After the real run,
 * `remaining_noncanonical` must equal the dry-run `unparseable` count (rows
 * deliberately left verbatim), and the articles row COUNT must be unchanged.
 *
 * Usage:
 *   node scripts/normalize-published-at.mjs --prod --dry-run   # stats only
 *   node scripts/normalize-published-at.mjs --prod             # execute
 */
import { d1, loadEnv, resolveAccount } from "./lib/cf-api.mjs";

const DRY_RUN = process.argv.includes("--dry-run");
const PROD = process.argv.includes("--prod");
if (!PROD) {
  console.error("Usage: node scripts/normalize-published-at.mjs --prod [--dry-run]");
  console.error("This backfill targets the PRODUCTION D1 only; --prod is required.");
  process.exit(2);
}

const env = loadEnv();
const DB = process.env.PROD_D1_DATABASE_ID || env.PROD_D1_DATABASE_ID;
if (!DB) {
  console.error("Missing PROD_D1_DATABASE_ID in .env files.");
  process.exit(2);
}

/** Canonical form: `YYYY-MM-DDTHH:MM:SSZ`, exactly 20 chars. */
const CANONICAL = /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$/;

/**
 * JS twin of `utils::normalize_published_at` (chrono): ① already-canonical /
 * RFC3339-ISO via Date.parse (idempotent), ② trailing textual zone
 * GMT/UT/UTC (case-insensitive) rewritten to +0000 then RFC822 via
 * Date.parse. Returns canonical string or null when unparseable.
 */
function canonicalize(raw) {
  if (raw == null) return null;
  const s = String(raw).trim();
  if (s.length === 0) return null;
  if (CANONICAL.test(s)) return s; // already canonical — idempotent
  let candidate = s;
  const upper = s.toUpperCase();
  for (const zone of ["GMT", "UT", "UTC"]) {
    if (upper.endsWith(zone)) {
      candidate = s.slice(0, s.length - zone.length) + "+0000";
      break;
    }
  }
  const ms = Date.parse(candidate);
  if (!Number.isFinite(ms)) return null;
  return new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z"); // whole seconds
}

/** Expected values are byte-identical to the Rust unit-test assertions. */
const SELF_CHECK = [
  ["Wed, 02 Sep 2026 12:00:00 GMT", "2026-09-02T12:00:00Z"],
  ["Wed, 02 Sep 2026 12:00:00 +0000", "2026-09-02T12:00:00Z"],
  ["Wed, 02 Sep 2026 12:00:00 -0500", "2026-09-02T17:00:00Z"],
  ["Wed, 02 Sep 2026 12:00:00 +0530", "2026-09-02T06:30:00Z"],
  ["Wed, 31 May 2023 07:00:00 GMT", "2023-05-31T07:00:00Z"],
  ["Thu, 03 Sep 2026 08:14:29 GMT", "2026-09-03T08:14:29Z"],
  ["2026-09-02T12:00:00Z", "2026-09-02T12:00:00Z"],
  ["2026-08-15T09:30:00+00:00", "2026-08-15T09:30:00Z"],
  ["2026-09-02T12:00:00-05:00", "2026-09-02T17:00:00Z"],
  ["", null],
  ["not a date", null],
];

function runSelfCheck() {
  const failed = [];
  for (const [input, expected] of SELF_CHECK) {
    const got = canonicalize(input);
    if (got !== expected) failed.push({ input, expected, got });
  }
  if (failed.length) {
    console.error("SELF-CHECK FAILED — JS normalizer diverges from Rust-locked values:");
    for (const f of failed) {
      console.error(`  input:    ${JSON.stringify(f.input)}`);
      console.error(`  expected: ${JSON.stringify(f.expected)}`);
      console.error(`  got:      ${JSON.stringify(f.got)}`);
    }
    console.error("Aborting: a divergence here means history would be rewritten inconsistently with the writer.");
    process.exit(1);
  }
  console.log(`Self-check PASS (${SELF_CHECK.length} cases locked to Rust unit-test values)`);
}

async function countArticles() {
  const rows = await d1(DB, "SELECT COUNT(*) AS n FROM articles");
  return rows[0].n;
}

async function fetchPublished() {
  // Non-NULL only: NULL published_at rows have no timestamp to normalize.
  return d1(DB, "SELECT id, published_at FROM articles WHERE published_at IS NOT NULL ORDER BY id");
}

/** Classify every row exactly once, mirroring the Rust normalizer's contract. */
function classify(rows) {
  const stats = { total: rows.length, already: 0, will: 0, unparseable: 0 };
  const todo = []; // { id, oldVal, newVal }
  const unparseable = []; // { id, published_at }
  for (const r of rows) {
    const val = String(r.published_at);
    if (CANONICAL.test(val)) {
      stats.already += 1;
    } else {
      const next = canonicalize(val);
      if (next === null) {
        stats.unparseable += 1;
        unparseable.push({ id: r.id, published_at: val });
      } else {
        stats.will += 1;
        todo.push({ id: r.id, oldVal: val, newVal: next });
      }
    }
  }
  return { stats, todo, unparseable };
}

function printStats(label, stats) {
  console.log(`  ${label}: total=${stats.total} already_normalized=${stats.already} will_normalize=${stats.will} unparseable=${stats.unparseable}`);
}

/** Bounded-concurrency UPDATE pool; one statement per row (D1 REST is one statement/call). */
async function updateRows(todo, limit = 8) {
  let i = 0;
  const workers = Array.from({ length: Math.min(limit, todo.length) }, async () => {
    while (i < todo.length) {
      const item = todo[i++];
      await d1(DB, "UPDATE articles SET published_at = ? WHERE id = ?", [item.newVal, item.id]);
    }
  });
  await Promise.all(workers);
}

async function main() {
  const account = resolveAccount();
  console.log(`Account: ${account}`);
  console.log(`Target (PROD): ${DB}`);
  console.log(`Mode: ${DRY_RUN ? "DRY-RUN (no writes)" : "EXECUTE"}\n`);

  runSelfCheck();

  const countBefore = await countArticles();
  const rows = await fetchPublished();
  const { stats, todo, unparseable } = classify(rows);

  console.log(`articles: total rows = ${countBefore}, non-NULL published_at = ${rows.length}\n`);
  console.log("Pre-run statistics (rows with published_at NOT NULL):");
  printStats("all rows", stats);

  // Eyeball samples of what a real run would rewrite (old -> canonical).
  if (DRY_RUN && todo.length) {
    console.log("\nSample rewrites (first 8):");
    for (const t of todo.slice(0, 8)) {
      console.log(`  [id=${t.id}] ${JSON.stringify(t.oldVal)}  ->  ${t.newVal}`);
    }
  }
  if (unparseable.length) {
    console.log(`\nUnparseable rows will be KEPT VERBATIM (no data loss): ${unparseable.length}`);
    if (DRY_RUN) {
      for (const u of unparseable.slice(0, 8)) console.log(`  [id=${u.id}] ${JSON.stringify(u.published_at)}`);
    }
  }

  if (DRY_RUN) {
    console.log("\n(dry-run finished — nothing was written; will_normalize rows above are what an execute run rewrites)");
    return;
  }

  // ---- EXECUTE ----
  console.log(`\nRewriting ${todo.length} rows…`);
  await updateRows(todo);
  console.log(`Done. Wrote ${todo.length} rows.`);

  // ---- Post-run verification ----
  const countAfter = await countArticles();
  const rowsAfter = await fetchPublished();
  const after = classify(rowsAfter);

  console.log("\nPost-run statistics (must reconcile with pre-run):");
  printStats("remaining non-canonical", after.stats);
  console.log(`count_before == count_after : ${countBefore} == ${countAfter}  ${countBefore === countAfter ? "OK" : "MISMATCH"}`);
  console.log(`remaining_noncanonical == dry-run unparseable : ${after.stats.will + after.stats.unparseable} == ${stats.unparseable}  ${after.stats.will + after.stats.unparseable === stats.unparseable ? "OK" : "MISMATCH"}`);

  let ok = true;
  if (countBefore !== countAfter) {
    console.error("FAIL: article row count changed — abort before trusting any data.");
    ok = false;
  }
  // Every row the dry-run classified as will_normalize must now be canonical,
  // and only the unparseable ones may remain non-canonical.
  if (after.stats.will !== 0 || after.stats.already !== countAfter) {
    console.error("FAIL: rows remain that were expected to be normalized.");
    ok = false;
  }
  if (after.stats.unparseable !== stats.unparseable) {
    console.error("FAIL: unparseable count changed (should equal pre-run unparseable).");
    ok = false;
  }
  if (!ok) process.exitCode = 1;
  else console.log("\nBackfill verified OK.");
}

main().catch((err) => {
  console.error("\nBackfill failed:", err.message);
  process.exit(1);
});

/**
 * RSS Intelligence — Production Data Plane Consolidation (data migration).
 *
 * Migrates feeds + articles from dev D1 (rss-db-dev) into production D1
 * (rss-db) so that rss-db becomes the ONLY source of truth.
 *
 * Identity rules (idempotent / repeatable):
 *  - feed identity  = canonical URL (lower-cased origin/host, trailing "/"
 *    stripped from path, query preserved — Google News URLs depend on query).
 *  - article dedup  = production constraints UNIQUE(hash) + UNIQUE(feed_id,guid)
 *    enforced via INSERT OR IGNORE.
 *
 * Usage: node scripts/migrate-production-data.mjs [--dry-run]
 * Writes MIGRATION_REPORT.md when executed.
 */
import fs from "node:fs";
import { d1, loadEnv, resolveAccount } from "./lib/cf-api.mjs";

const DRY_RUN = process.argv.includes("--dry-run");
const env = loadEnv();
const PROD_DB = process.env.PROD_D1_DATABASE_ID || env.PROD_D1_DATABASE_ID;
const DEV_DB = process.env.D1_DATABASE_ID || env.D1_DATABASE_ID;
if (!PROD_DB || !DEV_DB) {
  console.error("Missing DB ids. Set D1_DATABASE_ID / PROD_D1_DATABASE_ID in .env files.");
  process.exit(2);
}

function canonicalUrl(raw) {
  const s = String(raw ?? "").trim();
  if (!s) return "";
  try {
    const u = new URL(s);
    u.hash = "";
    u.protocol = u.protocol.toLowerCase();
    u.hostname = u.hostname.toLowerCase();
    if ((u.protocol === "http:" && u.port === "80") || (u.protocol === "https:" && u.port === "443")) u.port = "";
    if (u.pathname.length > 1 && u.pathname.endsWith("/")) u.pathname = u.pathname.replace(/\/+$/, "");
    return u.toString();
  } catch {
    return s.replace(/\/+$/, "");
  }
}

const read = (db, sql, params = []) => d1(db, sql, params);

async function main() {
  const account = resolveAccount();
  console.log(`Account: ${account}\nTarget(PROD): ${PROD_DB}\nSource(DEV):  ${DEV_DB}\nMode: ${DRY_RUN ? "DRY" : "EXECUTE"}\n`);

  const [prodCnt, devCnt] = await Promise.all([
    read(PROD_DB, "SELECT (SELECT COUNT(*) FROM feeds) f,(SELECT COUNT(*) FROM articles) a"),
    read(DEV_DB, "SELECT (SELECT COUNT(*) FROM feeds) f,(SELECT COUNT(*) FROM articles) a"),
  ]);
  const prodBefore = { feeds: prodCnt[0].f, articles: prodCnt[0].a };
  const devCounts = { feeds: devCnt[0].f, articles: devCnt[0].a };

  const [prodFeeds, devFeeds] = await Promise.all([
    read(PROD_DB, "SELECT id,url,title,status FROM feeds ORDER BY id"),
    read(DEV_DB, "SELECT id,url,title,status FROM feeds ORDER BY id"),
  ]);

  const prodByKey = new Map(prodFeeds.map((f) => [canonicalUrl(f.url), f]));
  const feedMap = new Map(); // devFeedId -> prodFeedId
  const missing = [];
  for (const f of devFeeds) {
    const existing = prodByKey.get(canonicalUrl(f.url));
    if (existing) feedMap.set(f.id, existing.id);
    else missing.push(f);
  }

  console.log("Feed identity (canonical URL):");
  for (const f of devFeeds) {
    console.log(`  dev#${f.id} ${f.title} -> ${prodByKey.get(canonicalUrl(f.url)) ? "exists in PROD" : "MISSING in PROD"}`);
  }

  let insertedFeeds = 0;
  for (const f of missing) {
    if (DRY_RUN) {
      console.log(`  [dry] would insert feed ${f.title} (dev#${f.id})`);
      continue;
    }
    const rows = await d1(
      PROD_DB,
      "INSERT INTO feeds (url,title,status,error_message,last_fetched_at) VALUES (?,?,?,?,?) RETURNING id",
      [f.url, f.title, "active", null, null]
    );
    const newId = rows[0].id;
    prodByKey.set(canonicalUrl(f.url), { id: newId });
    feedMap.set(f.id, newId);
    insertedFeeds += 1;
    console.log(`  -> inserted feed ${f.title} as PROD#${newId}`);
  }
  // ---- migrate articles (dedup via INSERT OR IGNORE on hash + (feed_id,guid)) --
  const ART = "guid,title,link,summary,description,content,published_at,pub_date,hash,created_at";
  let devArticles = 0;
  let attempted = 0;

  const migrateFeed = async (devFeed) => {
    const target = feedMap.get(devFeed.id);
    if (target === undefined && !DRY_RUN) {
      throw new Error(`no prod target for dev feed ${devFeed.id}`);
    }
    const rows = await read(
      DEV_DB,
      `SELECT ${ART} FROM articles WHERE feed_id = ? ORDER BY id`,
      [devFeed.id]
    );
    devArticles += rows.length;
    if (target === undefined || DRY_RUN) return; // dry-run (or no target) = count only
    const artCols = ART.split(",");
    const fullCols = ["feed_id", ...artCols];
    for (let i = 0; i < rows.length; i += 8) {
      const chunk = rows.slice(i, i + 8);
      const values = [];
      const placeholders = [];
      for (const a of chunk) {
        placeholders.push(`(${fullCols.map(() => "?").join(",")})`);
        values.push(
          target, a.guid, a.title, a.link, a.summary, a.description, a.content,
          a.published_at, a.pub_date, a.hash, a.created_at
        );
      }
      await d1(
        PROD_DB,
        `INSERT OR IGNORE INTO articles (feed_id,${ART}) VALUES ${placeholders.join(",")}`,
        values
      );
      attempted += chunk.length;
    }
  };

  for (const f of devFeeds) await migrateFeed(f);

  const after = await read(PROD_DB, "SELECT (SELECT COUNT(*) FROM feeds) f,(SELECT COUNT(*) FROM articles) a");
  const prodAfter = { feeds: after[0].f, articles: after[0].a };
  const deltaArticles = prodAfter.articles - prodBefore.articles;

  console.log("\n===== MIGRATION SUMMARY =====");
  console.log(`DEV (source)   : ${devCounts.feeds} feeds, ${devCounts.articles} articles`);
  console.log(`PROD (before)  : ${prodBefore.feeds} feeds, ${prodBefore.articles} articles`);
  console.log(`PROD (after)   : ${prodAfter.feeds} feeds, ${prodAfter.articles} articles`);
  console.log(`Feeds inserted : ${insertedFeeds}`);
  console.log(`Articles scanned: ${devArticles}; attempts: ${attempted}`);
  console.log(`Articles added to PROD: ${deltaArticles}`);
  if (!DRY_RUN && attempted) console.log(`Duplicates skipped by OR IGNORE: ${attempted - deltaArticles}`);

  const report = [
    "# RSS Intelligence — Data Migration Report",
    "",
    `Run at (UTC): ${new Date().toISOString()}`,
    `Account: ${account}`,
    `Source: ${DEV_DB} (rss-db-dev)`,
    `Target: ${PROD_DB} (rss-db)`,
    `Mode: ${DRY_RUN ? "dry-run" : "execute"}`,
    "",
    "| Metric | Value |",
    "|---|---|",
    `| DEV feeds | ${devCounts.feeds} |`,
    `| DEV articles | ${devCounts.articles} |`,
    `| PROD feeds before | ${prodBefore.feeds} |`,
    `| PROD articles before | ${prodBefore.articles} |`,
    `| PROD feeds after | ${prodAfter.feeds} |`,
    `| PROD articles after | ${prodAfter.articles} |`,
    `| Feeds inserted | ${insertedFeeds} |`,
    `| Dev articles scanned | ${devArticles} |`,
    `| Insert attempts | ${attempted} |`,
    `| Articles added to PROD | ${deltaArticles} |`,
    "",
    "## feed_id mapping (dev -> prod)",
    "",
    "```json",
    JSON.stringify(Object.fromEntries(feedMap), null, 2),
    "```",
    "",
  ];
  if (!DRY_RUN) fs.writeFileSync("MIGRATION_REPORT.md", report.join("\n"), "utf8");
  if (DRY_RUN) console.log("\n(dry-run finished — nothing was written)");
}

main().catch((err) => {
  console.error("\nMigration failed:", err.message);
  process.exit(1);
});

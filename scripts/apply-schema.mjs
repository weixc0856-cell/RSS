/**
 * Applies a SQL migration file to one or both D1 databases.
 * SQLite / D1 REST execute one statement per call, so statements are split on ";\n".
 *
 * Usage:
 *   node scripts/apply-schema.mjs migrations/005_feed_health_fetch_runs.sql --prod
 *   node scripts/apply-schema.mjs migrations/005_feed_health_fetch_runs.sql --prod --dev
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { d1, loadEnv } from "./lib/cf-api.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");

const fileArg = process.argv.find((a) => a.endsWith(".sql"));
const doProd = process.argv.includes("--prod");
const doDev = process.argv.includes("--dev");
if (!fileArg || (!doProd && !doDev)) {
  console.error("Usage: node scripts/apply-schema.mjs <file.sql> --prod [--dev]");
  process.exit(2);
}

const env = loadEnv();
const PROD_DB = process.env.PROD_D1_DATABASE_ID || env.PROD_D1_DATABASE_ID;
const DEV_DB = process.env.D1_DATABASE_ID || env.D1_DATABASE_ID;

const sql = fs.readFileSync(path.join(REPO_ROOT, fileArg), "utf8");
// Strip SQL comment lines first, then split on statement terminators.
const noComments = sql
  .split(/\r?\n/)
  .filter((l) => !l.trim().startsWith("--"))
  .join("\n");
const statements = noComments
  .split(";")
  .map((s) => s.trim())
  .filter((s) => s.length > 0);

async function apply(dbId, label) {
  console.log(`\nApplying ${fileArg} -> ${label} (${dbId})`);
  for (const stmt of statements) {
    try {
      await d1(dbId, stmt);
      console.log(`  ok: ${stmt.split(/\s+/).slice(0, 5).join(" ")} ...`);
    } catch (err) {
      const ignore =
        err.message.includes("duplicate column name") ||
        err.message.includes("already exists");
      console.log(`  ${ignore ? "skip" : "FAILED"}: ${err.message.split("\n")[0]}`);
      if (!ignore) process.exitCode = 1;
    }
  }
}

const targets = [];
if (doProd) targets.push(apply(PROD_DB, "PROD rss-db"));
if (doDev) targets.push(apply(DEV_DB, "DEV rss-db-dev"));
Promise.all(targets).then(() => {
  console.log(process.exitCode ? "\nschema apply finished with errors" : "\nschema apply finished");
});

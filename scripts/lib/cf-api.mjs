/**
 * Shared Cloudflare API helpers for repo maintenance scripts (migrations,
 * exports, diagnostics). Credential model:
 *   - A scoped `CLOUDFLARE_API_TOKEN` (process env, then .env files) talks to
 *     the raw REST API and supports bound-param D1 queries.
 *   - A `wrangler login` OAuth session (no scoped token) cannot hit the REST
 *     API, so param-less D1 queries are routed through the local wrangler CLI
 *     (`d1ViaWrangler`). See `d1` for the split.
 * Real resource IDs live in .env.local / .env.production (never committed).
 */
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(__dirname, "..", "..");

function parseEnvFile(p) {
  const map = {};
  if (!fs.existsSync(p)) return map;
  for (const raw of fs.readFileSync(p, "utf8").split(/\r?\n/)) {
    const line = raw.trim();
    const m = line.match(/^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$/);
    if (!m || line.startsWith("#")) continue;
    let v = m[2].replace(/\s+#.*$/, "").trim();
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) {
      v = v.slice(1, -1);
    }
    map[m[1]] = v;
  }
  return map;
}

export function loadEnv() {
  const local = parseEnvFile(path.join(REPO_ROOT, ".env.local"));
  const prod = parseEnvFile(path.join(REPO_ROOT, ".env.production"));
  const merged = { ...local, ...prod }; // production wins for duplicate keys
  return merged;
}

export function resolveAccount() {
  if (process.env.CLOUDFLARE_ACCOUNT_ID) return process.env.CLOUDFLARE_ACCOUNT_ID;
  const env = loadEnv();
  return env.R2_ACCOUNT_ID || env.CLOUDFLARE_ACCOUNT_ID;
}

/**
 * A scoped Cloudflare API token, if one is available (process environment, then
 * the merged .env files). Returns null when only the wrangler OAuth login
 * exists — see `d1` for how that case is handled.
 */
export function resolveToken() {
  if (process.env.CLOUDFLARE_API_TOKEN) return process.env.CLOUDFLARE_API_TOKEN;
  const env = loadEnv();
  return env.CLOUDFLARE_API_TOKEN || null;
}

/** The repo-local wrangler CLI entry (run via `node`, so no shell/.cmd layer). */
function wranglerCliPath() {
  const p = path.join(REPO_ROOT, "node_modules", "wrangler", "bin", "wrangler.js");
  if (!fs.existsSync(p)) {
    throw new Error(
      "Local wrangler not found (node_modules/wrangler). Run `npm install`, or set " +
        "CLOUDFLARE_API_TOKEN in the environment or a .env file."
    );
  }
  return p;
}

/**
 * Run a single param-less D1 statement through `wrangler d1 execute`.
 *
 * Cloudflare rejects the wrangler *OAuth* token when it is presented as a
 * Bearer token on the raw REST API, but accepts it through wrangler's own CLI,
 * so this is the path for machines that only have `wrangler login` (no scoped
 * API token). Statements that need bound params cannot go through the CLI and
 * require a scoped `CLOUDFLARE_API_TOKEN`.
 */
function d1ViaWrangler(dbId, sql) {
  const res = spawnSync(
    process.execPath,
    [wranglerCliPath(), "d1", "execute", dbId, "--remote", "--json", "--command", sql],
    { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 }
  );
  if (res.error) throw res.error;
  let parsed;
  try {
    parsed = JSON.parse(res.stdout);
  } catch {
    const tail = (res.stderr || res.stdout || "").toString().slice(-600);
    throw new Error(`wrangler d1 execute returned no parseable JSON: ${tail}`);
  }
  const block = parsed?.[0];
  if (block?.success === false) {
    throw new Error(
      `wrangler d1 execute failed: ${block.error || "unknown"}\nSQL: ${sql.slice(0, 200)}`
    );
  }
  return block?.results ?? [];
}

/** Run a single D1 query with positional params. Returns result rows (array). */
export async function d1(dbId, sql, params = []) {
  const token = resolveToken();
  if (!token) {
    if (params.length > 0) {
      throw new Error(
        "This D1 query uses bound params and needs a scoped CLOUDFLARE_API_TOKEN " +
          "(the wrangler OAuth fallback supports param-less SQL only)."
      );
    }
    return d1ViaWrangler(dbId, sql);
  }
  const account = resolveAccount();
  const res = await fetch(
    `https://api.cloudflare.com/client/v4/accounts/${account}/d1/database/${dbId}/query`,
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ sql, params }),
    }
  );
  const json = await res.json();
  if (!res.ok || json.success === false) {
    const detail = json.errors?.map((e) => e.message).join("; ") || JSON.stringify(json);
    throw new Error(`D1 query failed (${res.status}): ${detail}\nSQL: ${sql.slice(0, 200)}`);
  }
  // result[0].results holds rows for a single statement
  const first = json.result?.[0];
  if (first?.success === false) {
    throw new Error(`D1 statement failed: ${first.error || "unknown"}`);
  }
  return first?.results ?? [];
}

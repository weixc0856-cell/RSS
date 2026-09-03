/**
 * Shared Cloudflare API helpers for repo maintenance scripts (migrations,
 * exports, diagnostics). Reads credentials from:
 *   1. env CLOUDFLARE_API_TOKEN / CLOUDFLARE_ACCOUNT_ID
 *   2. wrangler OAuth config (~/.wrangler/config/default.toml)
 *   3. .env.local / .env.production (real resource IDs never committed)
 */
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
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

export function resolveToken() {
  if (process.env.CLOUDFLARE_API_TOKEN) return process.env.CLOUDFLARE_API_TOKEN;
  const cfgPath = path.join(os.homedir(), ".wrangler", "config", "default.toml");
  if (!fs.existsSync(cfgPath)) {
    throw new Error("No CLOUDFLARE_API_TOKEN and no wrangler default.toml found");
  }
  const text = fs.readFileSync(cfgPath, "utf8");
  const m = text.match(/^\s*oauth_token\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error("oauth_token not found in wrangler default.toml");
  return m[1];
}

/** Run a single D1 query with positional params. Returns result rows (array). */
export async function d1(dbId, sql, params = []) {
  const token = resolveToken();
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

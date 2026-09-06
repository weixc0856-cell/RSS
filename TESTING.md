# Testing

Test strategy for the RSS Worker + Astro frontend.

## 1. Unit tests (Rust, host)
Run: `cargo test --all`

| Module | Covers |
|---|---|
| `feed.rs` | RSS 2.0 / Atom parsing, entity+CDATA, guid fallback, malformed XML, md5 hash, nullable D1 binding helper; **write-contract**: `<pubDate>`/`<published>`/`<updated>` → canonical UTC ISO at the `parse_document` choke point (offset pubDate shifted to UTC and still sorted correctly), unparseable pubDate preserved verbatim, Atom fractional/offset timestamps collapse to whole seconds |
| `types.rs` | serde round-trips for all models & requests, ApiResponse shapes |
| `identity.rs` | `normalize_device_key` host cases: accepts a plain UUID, trims whitespace, rejects blank, rejects >128 bytes (abuse cap) / accepts exactly 128 |
| `utils.rs` | RFC3339 timestamp; **`normalize_published_at`** boundary table (RFC822 `GMT`/`UT`/`UTC`/`+0000`/`-0500`/`+0530`/`-0000`, RFC3339 `Z`/`+00:00`/`-05:00`/fractional seconds, unpadded day, whitespace), unparseable/zoneless → `None`, fixed-20-char shape + cross-encoding convergence invariants |
| `queue.rs` | FetchJob serde/parse/reject-malformed; `route_job`: v1 version/type
  contract, unknown-version/type rejection, retired `source_fetch` rejection |

Expected: `73 passed` (run `cargo test --all`; WS7: −2 removed Subscription /
SubscribeFeedRequest serde tests, +4 identity tests).

## 2. Integration + functional tests (live HTTP)
Run: `pwsh scripts/test-functional.ps1 -Base https://rss-worker.weixc0856.workers.dev`

Checks (assertive, exits non-zero on failure):
- health, diagnostics shape, feeds list
- `/api/sources` is a **retired API** (dormant prototype layer): `GET` and `POST`
  answer an honest 501 with `success:false` — no `X-User-Id` is required and
  nothing is read or created (the layer is not reachable through HTTP).
- WS7 device model (read-only — no subscribe/delete round-trips that would
  mutate the pool): OPTIONS preflight (allowed Origin + requested method/headers)
  re-advertises `X-User-Id` in `Access-Control-Allow-Headers` (the custom header
  makes every request preflight, so omitting it breaks the browser UI); and
  `GET /api/me/feeds` with a fixed device key returns `success:true` + an array
  (empty for a brand-new key — the first call provisions one benign profile
  row), while an anonymous request answers a structured 400.

### WS7 device-isolation drill (mutates — dev only)
Run: `node scripts/ws7-device-drill.mjs` (defaults to the dev worker URL)

Scripted equivalent of the prod incognito check: drives two fake devices (A/B)
through the full subscription lifecycle against the shared pool — anonymous 400,
empty start (no cross-device sync), add-form subscribe (`created:true`),
Discover subscribe (`POST /:id/subscribe`, idempotent), re-add convergence
(`already:true`), shared vs last-user unsubscribe (`pruned:false` / `pruned:true`
+ pool prune), structured 404 for a non-subscriber. **Mutates the pool** (creates
+ prunes one drill feed) and provisions two idempotent profile rows; refuses the
production host unless `WS7_DRILL_ALLOW_PROD=1`. Self-cleaning: a preamble
clears any leftover drill feed from a crashed prior run and the drill asserts the
pool returns to its pre-run snapshot, so it is safe to re-run.

### WS7.1 Discover catalog (harness = maintenance tool; drill = regression)

Run: `node scripts/ws7-1-validate-feeds.mjs` (maintenance, dev edge)
Run: `node scripts/ws7-1-catalog-drill.mjs` (dev functional regression)

- The **edge validation harness** (`ws7-1-validate-feeds.mjs`) is a maintenance /
  verification tool, NOT daily CI: it asks the DEV worker's real fetch path (true
  Rust parser + CF-edge reachability) to judge each official-source RSS candidate
  GREEN/AMBER/RED — HTTP ok + parse ok + ≥1 article + title + link-or-id +
  every non-null `published_at` canonical — and writes evidence to
  `ws7-1-verified.json`. Per candidate it transiently adds + prunes its own feed;
  refuses the production host. Re-run it when a catalog feed dies, to refresh the
  evidence before editing `recommended-feeds.ts`.
- The **catalog drill** (`ws7-1-catalog-drill.mjs`) is the WS7.1 regression:
  static gates (catalog non-empty, unique name/url, complete fields, pure-data
  module, **every shipped row has a GREEN verdict in `ws7-1-verified.json`**) plus
  an API round-trip that mirrors the Discover UI semantics exactly: Recommended
  "+" on device A → row turns "✓"; device B "+" on the same url converges on A's
  pool feed (`created:false`, no duplicate); a catalog-row pool feed renders once
  and is excluded from the Shared-pool tail; re-add is idempotent (`already:true`);
  last-user unsubscribe prunes; pool snapshot restored. Mutates the pool
  transiently (creates + prunes one feed); dev-only.

## 3. Performance sampling
Run: `pwsh scripts/test-perf.ps1 -Base <url> -Iterations 30`

Samples avg / p95 / max latency (ms) for: health, diagnostics, feeds list.

## 4. Cron / scheduling diagnostics
Production-only, best effort:
- heartbeat table `cron_ticks` records each scheduled invocation (`fired_at`),
- visible through `GET /api/diagnostics` (`cron_ticks`),
- configure `* * * * *` temporarily to confirm minute cadence (see commit history).

## 5. Frontend build test (Astro)
Run from `frontend/`: `npm run build` and `npm run preview` (HTTP 200 on `/`).

## 6. `published_at` canonical contract（数据面，2026-09-03）

Invariant（ARCHITECTURE.md §3.1）：字符串排序 ≡ 时间排序，仅当每行 `published_at` 都是
固定 20 字符 `YYYY-MM-DDTHH:MM:SSZ`。三层测试锁死它：

**a. Rust 单测（写入契约，随 §1 跑）**
- `utils::normalize_published_at_*`：完整变体表（RSS RFC822 `GMT`/`UT`/`UTC`/`±hhmm`/`-0000`、
  Atom RFC3339 `Z`/`±hh:mm`/小数秒、非补零 day、空白容忍）；不可解析/无时区 → `None`
  （保留原文，不猜时间）；固定 20 字符 + 跨编码同刻收敛。
- `feed::parse_rss_normalizes_pubdate_to_canonical_utc_iso` / `parse_atom_*` /
  `parse_rss_keeps_unparseable_pubdate_verbatim`：`parse_document` 收口端到端。
- 平台坑：收口里的失败日志在 wasm 走 `console_log!`，host 测试走 `eprintln!`
  （`log_normalize_failure` cfg 门控）——worker_sys 的 console 符号是 wasm import，host 直接
  调用会 abort。

**b. JS 回填自检（一次性脚本内）**
`scripts/normalize-published-at.mjs` 的 `SELF_CHECK` 锁 11 个输入 → 期望值与 Rust 单测断言
**字节一致**（chrono 与 V8 是两个解析器，分歧必须失败而非静默）。

**c. 生产只读契约检查（可重复）**
```bash
node scripts/check-articles-contract.mjs        # 只读；对 rss-worker-production + 生产 D1
```
断言：非 NULL `published_at` 全 canonical、feeds 总数 == `/api/feeds` 长度（动态比对，
不写死个数）、无重复 hash；`/api/health` `newest_published_at` canonical 且 <48h
（证明 `MAX(published_at)` 是真实时间序）；每 feed `/api/feeds/:id/articles` 50 条窗口
非空、全 canonical、DESC 单调。
> WS7 后 `/api/feeds` 是**共享池目录（Discover-only）**，非设备视图 —— 契约脚本的
> feeds==D1 计数等式必须继续读池目录。`<48h` 断言依赖池内有 ≥1 订阅源且 cron 已跑
> （订阅门）：上线后先订阅再跑契约，见 PRODUCTION_BASELINE.md 的 WS7 验收。
> 注：严格 `[0-9]`×16 括号 GLOB 被 D1 拒为 "pattern too complex"，SQL sanity 用等长 `?`
> 骨架；权威严格校验在回填脚本 `remaining_noncanonical` 与 Rust 写入契约单测里。

# Testing

Test strategy for the RSS Worker + Astro frontend.

## 1. Unit tests (Rust, host)
Run: `cargo test --all`

| Module | Covers |
|---|---|
| `feed.rs` | RSS 2.0 / Atom parsing, entity+CDATA, guid fallback, malformed XML, md5 hash, nullable D1 binding helper; **write-contract**: `<pubDate>`/`<published>`/`<updated>` → canonical UTC ISO at the `parse_document` choke point (offset pubDate shifted to UTC and still sorted correctly), unparseable pubDate preserved verbatim, Atom fractional/offset timestamps collapse to whole seconds |
| `types.rs` | serde round-trips for all models & requests, ApiResponse shapes |
| `utils.rs` | RFC3339 timestamp; **`normalize_published_at`** boundary table (RFC822 `GMT`/`UT`/`UTC`/`+0000`/`-0500`/`+0530`/`-0000`, RFC3339 `Z`/`+00:00`/`-05:00`/fractional seconds, unpadded day, whitespace), unparseable/zoneless → `None`, fixed-20-char shape + cross-encoding convergence invariants |
| `queue.rs` | FetchJob (legacy) & SourceJob (user) serde/parse/reject-malformed |

Expected: `63 passed` (run `cargo test --all`).

## 2. Integration + functional tests (live HTTP)
Run: `pwsh scripts/test-functional.ps1 -Base https://rss-worker.weixc0856.workers.dev`

Checks (assertive, exits non-zero on failure):
- health, diagnostics shape, legacy feeds list
- user-scoped `/api/sources`: create → duplicate conflict(409) → isolation (user B
  cannot see A) → list own → PUT update → POST fetch (worker fetch→parse→rss_articles)
  → GET articles non-empty with fields → delete by non-owner is no-op → owner delete
  removes source.

## 3. Performance sampling
Run: `pwsh scripts/test-perf.ps1 -Base <url> -Iterations 30`

Samples avg / p95 / max latency (ms) for: health, diagnostics, sources list, feeds
list, and (if present) source articles read.

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
断言：非 NULL `published_at` 全 canonical、feeds=3、无重复 hash；`/api/health`
`newest_published_at` canonical 且 <48h（证明 `MAX(published_at)` 是真实时间序）；
每 feed `/api/feeds/:id/articles` 50 条窗口非空、全 canonical、DESC 单调。
> 注：严格 `[0-9]`×16 括号 GLOB 被 D1 拒为 "pattern too complex"，SQL sanity 用等长 `?`
> 骨架；权威严格校验在回填脚本 `remaining_noncanonical` 与 Rust 写入契约单测里。

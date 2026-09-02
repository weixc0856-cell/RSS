# Testing

Test strategy for the RSS Worker + Astro frontend.

## 1. Unit tests (Rust, host)
Run: `cargo test --all`

| Module | Covers |
|---|---|
| `feed.rs` | RSS 2.0 / Atom parsing, entity+CDATA, guid fallback, malformed XML, md5 hash, nullable D1 binding helper |
| `types.rs` | serde round-trips for all models & requests, ApiResponse shapes |
| `utils.rs` | RFC3339 timestamp |
| `queue.rs` | FetchJob (legacy) & SourceJob (user) serde/parse/reject-malformed |

Expected: `28 passed`.

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

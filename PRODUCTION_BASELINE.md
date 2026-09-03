# Production Acceptance Baseline（生产验收基线）

> 只读冻结快照：本文件不执行任何写操作，仅记录 `rss-worker-production` 在某时刻的
> `/api/health` 状态，作为本轮改动落地的**改动前基线**。
>
> 快照非实时 —— 复测 / 验收请重新拉取，勿以此文件替代健康检查：

```bash
curl -sS https://rss-worker-production.weixc0856.workers.dev/api/health
```

## 快照（2026-09-03T03:43:34 UTC，`generated_at`）

| 维度 | 值 |
|---|---|
| environment | `production` |
| feeds.total | 6 |
| feeds.active | 3 |
| feeds.failed | 3 |
| articles.total | 1203 |
| articles.newest_stored_at | 2026-09-03 03:01:20（本轮改动前最后一次成功抓取） |
| scheduler.last_run.id | 5 |
| scheduler.last_run.status | `partial`（feeds_scheduled=5，feeds_fetched=2，feeds_failed=3，articles_inserted=0） |
| scheduler.last_run.started_at | 2026-09-03 03:30:38 |
| scheduler.last_run.finished_at | 2026-09-03 03:31:35 |
| scheduler.oldest_successful_feed_at | 2026-09-03 03:16:10 |
| cron | `*/15 * * * *`（wrangler.toml `[triggers]`，生产继承） |

说明：`last_run.status=partial` 与 `feeds.failed=3` 为**改动前旧代码**运行结果 ——
其中可能包含 `record_run` 的记账失真（本仓库本轮将修复的 bug 范围）。该快照仅作
"改动前"参照，不构成对健康度的结论。

## 原始响应

```json
{"success":true,"data":{"articles":{"newest_published_at":"Wed, 31 May 2023 07:00:00 GMT","newest_stored_at":"2026-09-03 03:01:20","total":1203},"environment":"production","feeds":{"active":3,"failed":3,"total":6},"generated_at":"2026-09-03T03:43:34.677+00:00","scheduler":{"last_run":{"articles_inserted":0,"feeds_failed":3,"feeds_fetched":2,"feeds_scheduled":5,"finished_at":"2026-09-03 03:31:35","id":5,"started_at":"2026-09-03 03:30:38","status":"partial"},"oldest_successful_feed_at":"2026-09-03 03:16:10"}},"error":null}
```

## 基线用途

- 改动落地并部署后，对比同一端点：feeds.active / feeds.failed、last_run 的
  `status` 与计数、articles 增速应能自洽解释（本仓库修复的累计记账 bug 会让
  `last_run` 的 `partial`/`ok` 判定更准确，而非消除失败本身）。
- 若需审计单 feed 明细：`GET /api/diagnostics`（`failed_feeds[]` + `last_fetch_run`）。

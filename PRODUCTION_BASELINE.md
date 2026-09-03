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

## 部署后复测（2026-09-03，修复版 v2 已上线）

部署历史（同日、同 commit 序列）：

- Pages production @ commit `775a5bd`（`feat(frontend)` 起 5 个 commit 推送 + 部署）。
- `rss-worker-production` v1 `49a72322`（03:55:08Z 激活）：record_run 采用"单条 UPDATE
  先增量、后在同一 SET 里按新列值定终态"，**依赖 SQLite SET 从左到右可见性 —— 在 D1 上不成立**
  （实测终态 CASE 读到的是更新前快照）。run #6（03:45，旧代码）与 run #7（04:00，v1）
  均未能说明问题；#7 两个 job 全回报后仍一直 `running`，直至 04:15 被调度器 supersede。
  该缺陷用本地 SQLite 精确复现（单语句法终态同为 `('running', 2, 0, None)`）。
- 修复 commit `369d5c3`：record_run 拆为两步 —— 先累计（`AND status='running'`），
  再按**已提交**累计数定终态（不再依赖 SET 顺序）。
- `rss-worker-production` v2 `85b8049e`（04:08:37Z 激活，commit `369d5c3`）。

修复版首个完整周期 run #8（混合路径 = 真实验收，`generated_at` 04:16:43 UTC）：

| 维度 | 值 |
|---|---|
| scheduler.last_run.id | 8 |
| scheduler.last_run.status | `partial`（feeds_scheduled=4，feeds_fetched=1，feeds_failed=3，articles_inserted=1） |
| scheduler.last_run.started_at / finished_at | 2026-09-03 04:15:37 / 04:16:34（约 57s 正常终结） |
| feeds.active / failed / total | 3 / 3 / 6 |
| articles.total | 1204（run #8 成功源新插入 1 篇） |
| 20s 后复查 | status 仍 `partial`、`finished_at` 不变（终态粘性成立） |

说明：run #8 对应 3 个 Google News 503 feed 到期重试失败 + 1 个成功源（新插入 1 篇）：
累计 4 = 4（fetched 1 + failed 3）即终结为 `partial`，与实际失败一致。
对比改动前 run #5 同为 `partial`（scheduled=5/fetched=2/failed=3）—— v2 下 run 可正常
终结且 `partial`/`ok` 判定不再受"最后一条 job 成败"干扰。

复测 / 对拍请重新拉取，勿以此文件替代健康检查：

```bash
curl -sS https://rss-worker-production.weixc0856.workers.dev/api/health
```

## 源清理（2026-09-03，run #8 之后）

运营决定：移除三条持续 HTTP 503 的 Google News RSS 搜索源。它们在本轮上线前的
03:01 曾短暂成功过一次（各入库一批文章），此后即被 Google 限流为 503；run #8 的
`failed=3` 正是这三条。

| id（已删） | title | URL（已删） |
|---|---|---|
| 4 | Anthropic Claude News | `https://news.google.com/rss/search?q=Anthropic+Claude&hl=en-US&gl=US&ceid=US:en` |
| 5 | Grok xAI News | `https://news.google.com/rss/search?q=Grok+xAI&hl=en-US&gl=US&ceid=US:en` |
| 6 | People's Daily English | `https://news.google.com/rss/search?q=site:en.people.cn&hl=en-US&gl=US&ceid=US:en` |

- 删除方式：D1 生产库直删 —— HTTP `DELETE /api/feeds/:id` 当前是 stub
  （见 [src/routes.rs:352](src/routes.rs#L352)），未走 API。先显式删子表再删 feeds，
  不依赖 D1 的 FK pragma。
- 连带删除：`articles` 共 82 篇（feed 4→28 / 5→29 / 6→25，均为 03:01 唯一一次成功
  抓取入库）；`subscriptions` 0、`rss_sources` 无同 URL 行，无其他引用。
- 删后（04:32 UTC 复核）：`feeds` total 6→**3**（active 3 / failed **0**）；
  `articles.total` 1204→**1122**。
- 删后首个 cron 周期 run #9：`ok`（scheduled=2/fetched=2/failed=0，04:30:38→04:31:14
  正常终结）。剩余 3 源健康；这也是修复版 v2 下的**首个全 `ok` run**
  （run #6 属旧代码，不作数）。

## 基线用途

- 改动落地并部署后，对比同一端点：feeds.active / feeds.failed、last_run 的
  `status` 与计数、articles 增速应能自洽解释（本仓库修复的累计记账 bug 会让
  `last_run` 的 `partial`/`ok` 判定更准确，而非消除失败本身）。
- 若需审计单 feed 明细：`GET /api/diagnostics`（`failed_feeds[]` + `last_fetch_run`）。

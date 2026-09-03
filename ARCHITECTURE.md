# RSS Intelligence — Architecture (vNext: Production Data Plane)

> 目标：把 RSS Intelligence 从“双环境、双数据源、弱状态可见”的实验系统，收敛成
> **“单一生产数据源 + 稳定数据身份 + 可观测抓取链路”** 的生产系统。

## 1. 事实架构图（2026-09-03 收口后）

```
                           ┌─────────────────────────────┐
                           │  rss-intelligence.pages.dev  │
                           │  Astro static SPA (no env    │
                           │  switch, no localStorage)    │
                           └──────────────┬──────────────┘
                                          │  GET/POST /api/*
                                          ▼
                   ┌──────────────────────────────────────────┐
                   │        rss-worker-production  ◄── 唯一 API │
                   │   rss-worker-production.weixc0856.workers.dev
                   └──────┬──────────────────────────┬─────────┘
                          │                          │  RSS_FETCH_QUEUE
                          ▼                          ▼  (rss-fetch-queue-prod)
                    D1  rss-db                  Queue consumer
                    (唯一事实来源)                  (同一 Worker)
                          │                          │
                          │        ┌─────────────────┘
                          ▼        ▼
                     RSS Sources / HTTP 抓取 → 解析 → 去重 → 持久化
```

开发/测试环境（禁止被前端引用）：

```
rss-worker (rss-worker.weixc0856.workers.dev)  →  D1 rss-db-dev   （默认/开发）
rss-worker-development                         →  D1 rss-db-dev   （--env development）
```

规则：
- **`rss-db`（生产 D1）是 RSS Intelligence 唯一数据事实来源。**
- 前端只调用 `rss-worker-production`；环境选择属于 **部署配置**，不是用户 UI 状态。
- Dev 数据仅用于开发/测试，不再与线上共享。

## 2. Worker / D1 / Queue / Cron 映射

| 名称 | workers.dev | D1 | Queue | Cron | 角色 |
|---|---|---|---|---|---|
| `rss-worker` | rss-worker.weixc0856.workers.dev | rss-db-dev | rss-fetch-queue (producer+consumer) | `*/15 * * * *` | 开发默认环境（保留，仅供本地/测试） |
| `rss-worker-development` | rss-worker-development.weixc0856.workers.dev | rss-db-dev | rss-fetch-queue (producer) | 无 | `--env development` 产物 |
| `rss-worker-production` | rss-worker-production.weixc0856.workers.dev | **rss-db** | rss-fetch-queue-prod (producer+consumer) | `*/15 * * * *` | **生产数据平面（唯一）** |

## 3. 数据身份

- Feed identity：`normalized_url`（`src/utils.rs::canonical_url`，唯一索引
  `uq_feeds_normalized_url`）。新建 feed 前先按 canonical URL 查重。
- Article 去重：沿用 `UNIQUE(hash)` + `UNIQUE(feed_id, guid)`（INSERT OR IGNORE），
  迁移与抓取共用同一套去重语义。
- 三个时间语义：`published_at`（内容方声明）、`last_fetched_at`（尝试抓取）、
  `last_success_at`（最后一次成功）、`next_fetch_at`（下次应抓时间）。
  UI 中文章时间显示 `published_at`；系统状态显示 `last_success/next_fetch`。

## 4. Feed 健康与抓取治理（schema 005）

`feeds` 新增：`normalized_url / enabled / fetch_interval_minutes / last_success_at /
last_failure_at / last_http_status / consecutive_failures / next_fetch_at / etag /
last_modified`。

- 成功：`last_success_at=now`、`consecutive_failures=0`、`next_fetch_at=now+interval`，
  存储响应的 `ETag / Last-Modified`。
- 失败（HTTP/解析错误）：`last_failure_at=now`、`error_message`、`last_http_status`、
  `consecutive_failures+=1`、`next_fetch_at=now+backoff`（指数退避，封顶 24h）。
- 条件请求：有 `etag/last_modified` 时携带 `If-None-Match / If-Modified-Since`；
  `304` 视为成功但不解析。

`fetch_runs` 记录每个调度周期的完整结果：

| 列 | 含义 |
|---|---|
| run_key | 分钟级幂等键（同分钟重复触发不会重复入队） |
| trigger | `cron:<expr>` |
| feeds_scheduled | 本次入队的 feed/source 数量 |
| feeds_fetched / feeds_failed | 队列消费端回写 |
| articles_inserted | 实际新插入文章数 |
| status | running / ok / partial / failed |
| finished_at | 所有 job 回报后置位 |

## 5. Scheduler 语义

- Cron（`*/15 * * * *`）只唤醒 Scheduler。
- Scheduler 按 `enabled=1 AND (next_fetch_at IS NULL OR next_fetch_at<=now())` 选择到期 feed，
  不再无条件全量抓取。
- 每条 job 携带 `run_id`；消息以 JSON 字符串发送（避免 workers-rs 对象字段丢失的坑），
  消费端 `normalize_body()` 兼容对象/字符串两种投递形态。
- **Queue payload 契约（v1）**：job 携带 `version: 1` + `type`（`feed_fetch` / `source_fetch`）。
  消费端对 `type` 存在但 `version != 1`（或缺失）或未知 `type` 的消息**显式拒绝**（不按 v1 处理，
  记一次失败并丢弃），保证未来 v2 不会被静默误处理；无 `type` 的在途旧消息按形状回退分派。
- **终态粘性**：`fetch_runs` 记帐 UPDATE 带 `AND status='running'` 守卫且按**累计**计数
  （`>= feeds_scheduled`）判定，迟到/重复/retry job 不会把已终结或被 supersede 的 run 翻案；
  有失败即 `partial`/`failed`，最后一条 job 成功也不会把整场 run 标成 `ok`。

## 6. API 可观测性

- `GET /api/health`：environment、feeds(active/failed)、articles、scheduler.last_run、
  oldest_successful_feed_at —— 用于区分“新闻本身旧”与“抓取系统旧”。
- `GET /api/diagnostics`：原字段 + `failed_feeds` 健康明细 + `last_fetch_run`。
- `GET /api/feeds`：直接暴露健康字段供 UI 展示。

## 7. 本阶段已完成

- [x] 链路修复：Cron→Queue→Fetch 真实打通（JSON 字符串投递 + 消费端 normalize）。
- [x] 数据迁移 dev→prod：6 feeds / 1120 articles（去重 108），见 `MIGRATION_REPORT.md`。
- [x] 前端收口：移除 Dev/Prod 切换与 `localStorage rss-env`；只连生产 Worker。
- [x] Schema 005（feed 健康 + fetch_runs）+ Rust 语义（条件请求、退避、next_fetch_at、fetch_runs 回写）。
- [x] Cron `*/15`，Worker 双环境部署，Pages 已发布。
- [x] `/api/health` 上线。
- [x] Queue job 版本化 + 消费端 `version` 校验（v2 显式拒绝，不静默按 v1 处理）。
- [x] `fetch_runs` 记帐修复：累计计数 + `status='running'` 守卫（终态粘性，见 §5）。
- [x] 304 保留 `etag/last-modified`：条件请求在下次抓取不退化回全量 GET。
- [x] 前端 Feed 级健康卡片（每 feed 状态行 + dot 色，`retry in …` / `last …`）。
- [x] 生产验收基线冻结：见 `PRODUCTION_BASELINE.md`。

## 8. 待办 / 后续

- [ ] 为 `rss_sources`（用户级源）补同一套健康字段并接入 UI 管理。
- [ ] 数据迁移脚本参数化 DB id 后入库（当前读 `.env`）。
- [ ] 模块拆分（api/fetcher/parser/persistence）为可选重构，不阻塞业务。
- [ ] CI：worker deploy + pages deploy workflow 固化（当前仅 rust.yml）。

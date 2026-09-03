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
```

> 2026-09-03：删除 `rss-worker-development`（`--env development` 产物，曾与 `rss-worker`
> 重复绑定同一 dev D1/queue，无 cron、非 queue consumer，从不运行）。dev 实例仅保留
> 默认环境的 `rss-worker`。

规则：
- **`rss-db`（生产 D1）是 RSS Intelligence 唯一数据事实来源。**
- 前端只调用 `rss-worker-production`；环境选择属于 **部署配置**，不是用户 UI 状态。
- Dev 数据仅用于开发/测试，不再与线上共享。

## 2. Worker / D1 / Queue / Cron 映射

| 名称 | workers.dev | D1 | Queue | Cron | 角色 |
|---|---|---|---|---|---|
| `rss-worker` | rss-worker.weixc0856.workers.dev | rss-db-dev | rss-fetch-queue (producer+consumer) | `*/15 * * * *` | 开发默认环境（保留，仅供本地/测试） |
| `rss-worker-production` | rss-worker-production.weixc0856.workers.dev | **rss-db** | rss-fetch-queue-prod (producer+consumer) | `*/15 * * * *` | **生产数据平面（唯一）** |

> `rss-worker-development` 已于 2026-09-03 删除（原为 `--env development` 产物，见 §1）。

## 3. 数据身份

- Feed identity：`normalized_url`（`src/utils.rs::canonical_url`，唯一索引
  `uq_feeds_normalized_url`）。新建 feed 前先按 canonical URL 查重。
- Article 去重：沿用 `UNIQUE(hash)` + `UNIQUE(feed_id, guid)`（INSERT OR IGNORE），
  迁移与抓取共用同一套去重语义。
- 三个时间语义：`published_at`（内容方声明）、`last_fetched_at`（尝试抓取）、
  `last_success_at`（最后一次成功）、`next_fetch_at`（下次应抓时间）。
  UI 中文章时间显示 `published_at`；系统状态显示 `last_success/next_fetch`。
- **数据平面 = `feeds` + `articles`**（抓取器写、API 读、前端调）；`rss_sources` /
  `rss_articles` 是 dormant 用户源原型层（0 行属预期），前端从不调用 —— 不是数据丢失。

### 3.1 `published_at` 排序契约（不变量，2026-09-03）

- **字符串排序 ≡ 时间排序，仅当**每行 `published_at` 都是单一固定格式
  `YYYY-MM-DDTHH:MM:SSZ`（UTC、秒级、`Z` 结尾、20 字符定长）。`ORDER BY published_at DESC`
  与 `/api/health` 的 `MAX(published_at)` 都依赖此契约。
- 抓取在 `parse_document` 唯一收口经 `utils::normalize_published_at` 归一：RSS RFC1123/RFC2822
  `pubDate`（`GMT`/`±hhmm`…）与 Atom RFC3339/ISO（`Z`/`±hh:mm`，幂等）一律转 UTC ISO。
  不可解析输入**保留原文并 `console_log`**（不丢数据，但该行无排序保证）。
- 历史回填 = `scripts/normalize-published-at.mjs`（一次性；先部署含归一化的 Worker 再回填，
  避免回填后旧 Worker 又写回原文）。JS 回填输出与 Rust 输出字节一致（自检表锁 Rust 单测值）。
- code review：把 feed 原文直接写入 `published_at`（绕过归一化）视为违约。

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
- **记帐分两条语句**：先 commit 本 job 计数（增量 UPDATE），再按**已提交**累计数定终态。
  单条 UPDATE 在同一 SET 里"先增量、后读新列"依赖 SQLite 从左到右可见性，在 D1 上读到的是
  更新前快照、终态永不触发（见 `PRODUCTION_BASELINE.md` 部署后复测），故不得合并回单条。

## 6. API 可观测性

- `GET /api/health`：environment、feeds(active/failed)、articles、scheduler.last_run、
  oldest_successful_feed_at —— 用于区分“新闻本身旧”与“抓取系统旧”。
- `GET /api/diagnostics`：原字段 + `failed_feeds` 健康明细 + `last_fetch_run`。
- `GET /api/feeds`：直接暴露健康字段供 UI 展示。

### 6.1 动态 API 响应头部（v1.1：no-store + CORS allow-list）

集中收尾点 `apply_api_headers()`（`src/lib.rs`）作用于每个动态 `/api/**` 响应与 OPTIONS
预检，兼管缓存与 CORS：

- **`Cache-Control: no-store`**：所有动态 API 响应默认不缓存。此前 API 无明确缓存策略，
  浏览器/中间层/CDN 的启发式缓存行为不确定，存在“旧状态（如早期空 feed 列表）冒充当前状态”
  的风险；`no-store` 把这一变量消除。将来若引入真正静态的 API（如 `/api/static-metadata`）
  可另行放宽，故表述为“默认 no-store”，不写死为永久义务。
- **`Vary: Origin`**：ACAO 依据请求 Origin 动态回显，响应代表的是“该 Origin 视角”的授权，
  任何缓存该响应的层都必须按 Origin 键分。
- **CORS allow-list（精确匹配，无 `*`、无通配端口/主机）**：允许集 =
  `https://rss-intelligence.pages.dev`、`http://localhost:4321`、`http://127.0.0.1:4321`。
  有 `Origin` 且命中 → 回显该 origin；无 `Origin`（curl、Worker 内部 scheduler/queue）或
  未命中 → 不设 `Access-Control-Allow-Origin`。**CORS 是浏览器访问控制，不是 API 认证**：
  服务端/curl 调用不受影响，`is_allowed_origin` 单测覆盖前缀/端口/宿主绕过负例。
- `Access-Control-Allow-Headers: Content-Type, X-User-Id`、
  `Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS`、`Max-Age: 86400`。
- **API 错误 ≠ 空数组**：`list_feeds`/列表类错误路径返回 500，不伪装成 `[]`；真空表返回的
  `[]` 是真“空”，前端据此区分“错误（可重试）”与“无数据”。

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
- [x] 上线落地（2026-09-03）：Pages @ `775a5bd` 推送并部署；`rss-worker-production`
  首版 v`49a72322` 暴露 record_run 单条 UPDATE 在 D1 上不终结的缺陷（run #7 卡 `running`），
  修复 commit `369d5c3` 二次部署为 v`85b8049e`（04:08:37Z 激活）；修复版首个完整周期
  run #8 `partial`（scheduled=4/fetched=1/failed=3/inserted=1，正常终结且终态粘性），
  见基线「部署后复测」。
- [x] **v1.1 可靠性收口（2026-09-03）**：动态 `/api` 默认 `no-store` + CORS 固定 allow-list
  与 `Vary: Origin`（§6.1）；默认源一次性 bootstrap（migration 006，见下）；前端三态
  （loading/empty/error）+ 按区 Retry + `ApiError.code` 网络层区分；diagnostics 降级为
  辅助信息（失败不抢主视觉）。删除 4/5/6 号不可用源并清 82 条关联文章（HTTP DELETE 是
  存根，删除经 D1 SQL 直连）。空库自动建默认源永远在后端/migration，不在前端。
- [x] **v1.1 上线落地（2026-09-03）**：commit `e9483af`（lib.rs）推送并部署，
  `rss-worker-production` 部署版本 `310816af`（05:11:14Z 激活）；Pages @ `e8d511c`
  构建并部署；006 对生产 apply 为 no-op。curl 验证：`no-store` + `Vary: Origin` 存在于
  `/api/feeds`、`/api/health`、`/api/feeds/:id/articles`；pages.dev Origin 回显 ACAO、
  `evil.example` 不设 ACAO、无 Origin 的 GET 正常返回 3 源。见基线「v1.1 可靠性收口」。

- [x] **`published_at` 归一化（2026-09-03）**：`articles.published_at` 统一为 canonical UTC
  ISO（§3.1 不变量）。Rust `normalize_published_at` + `parse_document` 写入收口（commit
  `cb6bcde`）+ 历史回填脚本（commit `19512e6`）。先部署 Worker（版本 `6ddaec69`）后回填
  （1148→1148，remaining_noncanonical=0）；部署后手动抓取证明新插入行同为 canonical。见
  `PRODUCTION_BASELINE.md`「published_at 归一化」增补。

### 7.1 默认源一次性 bootstrap（006，非 reconcile）

`migrations/006_default_feeds.sql` 幂等地种入当前 3 个健康源（NYT World / BBC News /
OpenAI News，`fetch_interval_minutes=15`、`enabled=1`、`next_fetch_at=NULL`）。三条语义契约：

1. **one-time bootstrap，不是 reconcile**：`WHERE NOT EXISTS` 按 `normalized_url` 守卫，
   只对“缺该源”的库补种；用户删除的源不会因重复执行 migration 自动复活。
2. **`next_fetch_at = NULL` 依赖 scheduler 既有 NULL-as-due**（§5 选源条件
   `enabled=1 AND (next_fetch_at IS NULL OR …)`），种下后首个 cron 即抓取。
3. 对当前生产是 no-op（3 源已存在）。**空库自动加源只发生在后端/migration，前端不参与**。

## 8. 待办 / 后续

- [ ] 为 `rss_sources`（用户级源）补同一套健康字段并接入 UI 管理。
- [ ] 数据迁移脚本参数化 DB id 后入库（当前读 `.env`）。
- [ ] 模块拆分（api/fetcher/parser/persistence）为可选重构，不阻塞业务。
- [ ] CI：worker deploy + pages deploy workflow 固化（当前仅 rust.yml）。

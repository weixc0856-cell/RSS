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
  **方向定稿（2026-09-06）：feeds/articles 是唯一生产模型**。用户源原型层不再演进，
  WS5（同日）已授权落地为 **code retired / data dormant**：运行期读写该层的代码与
  用户身份模型已退休删除，`/api/sources` 收口成诚实 501 retired API；
  `rss_sources`/`rss_articles` **表与数据逐字节保留**（不 drop、不迁移），
  diagnostics 仍读 dormant 计数（见 §7 WS5）。

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

### 3.2 设备模型：每设备一份订阅列表 + 共享池（WS7，2026-09-06）

**方向反转声明**：WS1 曾把 `DELETE /api/feeds/:id` 定为**全局删除**、WS6 的 ✕ 文案写
「shared catalog」——WS7 **有意反转二者为「设备退订」**，是新决策而非回归（见 §7 WS7）。
用户需求：其他设备访问不应是共享的同一批默认源；每设备各自一份列表、**可独立增删**。

- **共享池不变**：`feeds`/`articles` 仍是唯一的全局数据平面（一个源一次抓取、所有订阅者读
  同一批文章）。**不引入按设备的文章桶**。`subscriptions`（001 已存在）把
  `user_id → feed_id` 挂到池上；`UNIQUE(user_id, feed_id)` 保证幂等。
- **`profiles` = 匿名设备命名空间注册表（anonymous device namespace registry），不是用户/
  账户表**（migration 007）。它把浏览器随机 key（`X-User-Id` 头）稳定映射成
  `subscriptions.user_id` 需要的 INTEGER，纯命名空间用途：无鉴权、无账户生命周期、无
  device management、行从不删除。保留它是**有意设计**（历史 `user_id INTEGER` 免改列型；
  未来若真加 authentication 可在其上再叠账户层），但绝不命名/注释成 users。
- **`X-User-Id` 是 device namespace identifier —— ≠ authentication，≠ authorization**：
  128-bit 随机 UUID 只意味着「不同浏览器随机生成发生碰撞的概率极低」，**不意味着不可伪造**；
  任何人可发 `X-User-Id: abc`，若知道他人 key 即进入对应 namespace。这是隔离到浏览器粒度的
  **命名空间隔离，不是安全边界**。此措辞在 ARCHITECTURE/`src/identity.rs`/migration 007
  注释统一。前端 localStorage key = `rss_device_key`（`crypto.randomUUID` + 非安全上下文回退；
  localStorage 被禁时退化为会话内存 key，刷新即新设备，可接受）。
- **API 双面（不变式）**：`GET /api/feeds` = 共享池目录，**Discover-only** —— 前端「我的列表」
  导航**绝不允许**从它派生；`GET /api/me/feeds`（需 X-User-Id）= 本设备列表，
  **navigation-only**，返回与 `list_feeds` **同一 17 列投影**（共享 `FEED_PROJECTION` 常量）
  + `subscribed_at` + `article_count`（投影漂移会静默劣化导航健康徽标）。缺头 → 400
  `X-User-Id header required`。
- **池状态机**：某源订阅数 0 = **dormant**（不抓取、Discover 仍可见、留在池）；≥1 = **active**
  （一次抓取共享文章）；最后一个订阅者退订 → 该源 feed + 全部文章**从池 prune**（显式删序
  先 articles 后 feeds，不依赖 D1 FK pragma）。
- **抓取双门**：scheduler 只选「有 ≥1 订阅」的 due feed（§5）；消费端 `fetch_feed(url, env)`
  执行前第一步 feed 查找 SELECT 同时要求 `EXISTS(subscription)`（feed 被删即查不到、出网前
  短路），并在解析成功、落库前**再复查一次**订阅存在（封住出网那几秒内被 prune 的孤儿
  窗口；0 订阅则跳过落库返回 `Ok(0)`，不动已删行）。不做事务/reconcile（评审认可的边界）。
- **添加 = find-or-create + 订到本设备，幂等全成功**：URL 规范化查池 → 池无 → 建源 + 订阅 +
  初始抓取 enqueue（先订阅后 enqueue，保证执行门通过）；池有且本设备未订 → 静默订阅；已订 →
  no-op。全分支 `success:true`，data = `{feed, created, already}`。**✕ = 退订本设备**
  （返回 `{id, pruned}`）；`POST /api/feeds/:id/subscribe` = Discover「+」幂等订阅既有池源。

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
- **订阅门（WS7）**：选择条件追加 `EXISTS (SELECT 1 FROM subscriptions s WHERE
  s.feed_id = f.id)` —— 0 订阅的池源是 dormant，不入队不抓取（Discover 仍可见）；
  消费端另有执行门双保险（§3.2）。
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
  WS5 曾随 `/api/sources` 退休把 `X-User-Id` 从 CORS 摘除（当时无端点读它）；WS7 为设备
  命名空间**重新宣告** `X-User-Id`（`/api/me/feeds`、POST `/api/feeds`、subscribe、
  DELETE 均读它）—— 自定义头使每个请求都走预检，漏宣告则整站 CORS 挂（functional 测试
  有断言）。
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

- [x] **共用硬伤收口 + 方向定稿（2026-09-06）**：四条与方向无关的硬伤修复落地，并正式
  定稿「feeds 即产品」（§3）。顺序 WS1→WS4→WS2→WS3，各自独立 commit，均在 Gate 1
  native 73 tests / wasm `cargo check --tests` / 前端 build+typecheck 通过后提交。
  - **WS1** `320fb17`：`DELETE /api/feeds/:id` 真实现为**全局删除**（显式
    subscriptions → articles → feeds 三删，404 `Feed not found`），**不是**取消订阅语义；
    三个 dead `subscriptions` stub 改诚实 501（`success:false`，不写任何 D1）；
    `delete_source` 加属主检查（无主 → 404 `Source not found`）。
  - **WS4** `43ffd50`：diagnostics 纯增量纳入 `rss_sources`/`rss_articles` 计数
    （生产 0/0，仅叠加，既有字段不动）。
  - **WS2** `5700201`：feed/source 创建即 best-effort 入队首抓（payload 取自入库行，
    非请求串再推导）；入队失败 `console_error` 带 id；**重复入队可接受**（文章持久化幂等，
    outbound 不去重），`next_fetch_at=now` 保证 cron 兜底。
  - **WS3** `b0ade57`：两管线共用**硬化 outbound fetch**（transport hardening，非 fetch
    policy rewrite）—— 单跳 20s 响应超时（deadline，非硬取消；worker-rs 0.8.5 无 signal，
    wasm 侧 `Delay` 竞速、非 wasm 直通）、2 MiB 体积上限（**post-buffering validation**，
    非预流式内存上限）、词法 SSRF 守卫 `utils::is_safe_fetch_url`（**lexical guard，非完整
    SSRF 防护**：DNS rebinding 不在覆盖内；redirect 每一跳复查 —— 真正风险是
    `https://trusted → 302 http://127.0.0.1`）、redirect 上限 5（无 Location 的 3xx 视为
    终态非 2xx，不进 body）；`FetchedFeed.body` 仅 2xx 有值（304 恒空 body 不解析）。
  - 维护 `d675239`：`cf-api.mjs` D1 认证修复 —— scoped `CLOUDFLARE_API_TOKEN` 走 REST；
    仅 OAuth 时无参查询改走本地 `wrangler d1 execute`（REST 拒收 OAuth token），带参查询
    明确报错；`check-articles-contract.mjs` 去掉过时 `feeds==3` 硬基线，改跨端一致性 +
    遍历全部 feed（详见脚本头）。
  - **Gate 2 验收**：生产 legacy diagnostics **逐字段不变**（含 `last_fetch_run.id` 同 run）、
    `POST /api/feeds/2/fetch` 回归 200、全 feed 文章窗口 canonical + DESC；D1 标量
    nulls/noncanon/dups 均 0。dev 冒烟（创建即首抓、全局删除、重复删除 404、501、sources
    属主隔离 404、diagnostics 新增字段）全绿。
  - 部署：生产 `rss-worker-production` v`55782b68`、dev `rss-worker` v`e11df86b`。

- [x] **WS5 源层退休（2026-09-06）**：dormant prototype layer 收口为 **code retired /
  data dormant**（commit `79ae355`，`refactor(api): retire user-scoped /api/sources`）。
  方向沿用「feeds 即产品」（§3）。统一词汇：`rss_sources`/`rss_articles` = dormant
  prototype layer、`/api/sources` = retired API、source pipeline = retired。
  - **HTTP**：lib.rs 六条 source dispatch 分支 → 一条语义边界 guard
    （`p == "/api/sources" || p.starts_with("/api/sources/")`，非裸前缀）→
    `handle_sources_retired()`：任意 method/子路径一律 **501 `success:false`**、
    **绝不写 D1**（retired API，非 half-usable），无需 `X-User-Id`；走 match 内统一
    `apply_api_headers`（no-store/CORS 照常）。`Access-Control-Allow-Headers` 摘除
    `X-User-Id`（已无端点读它，见 §6.1）。
  - **运行期**：queue 删 `SourceJob` / `RoutedJob::Source` 变体 / consume Source 臂；
    `route_job` 对 typed `source_fetch` 与 legacy `{source_id,user_id}` 形状一律
    **显式退休拒绝**（Unsupported）；scheduler 删 rss_sources 选择 + source_fetch
    入队臂。**在途历史 source_fetch 记 1 failed** = 既有 Unsupported 契约（使所属 run
    能终结），注释明示 `feeds_failed` 含 rejected/unsupported/retired —— 真实网络抓取
    失败活在 feed 行（`error_message`/`consecutive_failures`），不在此计数。
  - **Retired code 删除**：`src/sources.rs`（450 行）+ `src/auth.rs` + lib.rs `mod`
    声明 + types.rs 源层类型（`SourceItem`/`CreateSourceRequest`/`RssArticle` 等）。
    auth 身份模型（`current_user`/`X-User-Id`）删除前已做**全仓引用验证**（Rust/
    frontend/scripts/migrations/docs 均零残留调用）。feeds 层死订阅 stub（WS1 已
    501）与 feeds/articles 运行期代码不受影响。
  - **Gate**：native **71 tests**（删 4、增 2：typed + legacy source_fetch 退休拒绝
    单测锁死行为）；wasm `cargo check --tests` 干净；前端零改动 build+typecheck 通过。
    部署后 Gate 2：生产/开发 `/api/sources` 全 method 501、`/api/feeds` `/api/health`
    `/api/diagnostics` 不受影响；diagnostics 仍读 dormant 表计数（生产 0/0 不变，dev
    历史行停止刷新）；生产 `check-articles-contract.mjs` 全 PASS；OPTIONS 预检
    `Allow-Headers` 仅 `Content-Type`。
  - 部署：生产 `rss-worker-production` v`ae54114e`、dev `rss-worker` v`1f6d1a8f`。

- [x] **WS7 设备模型（2026-09-06）**：每设备一份订阅列表 + 共享 feeds/articles 池（§3.2）。
  方向反转声明：WS1 的「全局删除」与 WS6 的「shared catalog ✕ 文案」在此**有意反转**为
  「设备退订」—— WS6 的 ✕ 交互链路（hover/键盘可达、能加→能看→能删）全部保留，只改语义
  与文案。三 commit：A `e61a4b5`（worker+schema）→ B `443a588`（frontend）→ C（scripts/
  docs，本记录所在 commit）。各 commit 均过 Gate 1：native **73 tests**（WS5 的 71 − 2
  删 Subscription/SubscribeFeedRequest serde + 4 增 identity）、wasm `cargo check --tests`、
  前端 build+typecheck、死臂净零 grep（`handle_get_user_feeds`/`handle_subscribe_feed`/
  `handle_unsubscribe_feed`、`Subscription`/`SubscribeFeedRequest` 全仓零残留）。
  - **schema**：`migrations/007_device_profiles.sql` —— `profiles` = 匿名设备命名空间
    注册表（`device_key` UNIQUE → INTEGER id）。非用户/账户表、无鉴权；行从不删除。生产
    subscriptions 现为 0 行，007 无需回填。**上线先 apply 007 再部署 worker**（否则引用
    不存在的表）。
  - **worker**：`src/identity.rs`（`normalize_device_key` ≤128 字节 abuse guard +
    `require_profile` 按 key 查/插 profiles，唯一键竞态重试一次）；routes 抽
    `FEED_PROJECTION` 常量供 list_feeds 与 `handle_get_my_feeds` 共用同投影；POST
    `/api/feeds` 翻转为幂等 find-or-create + 订到本设备（`{feed, created, already}` 全成功）；
    新增 `GET /api/me/feeds`（navigation-only）与 `POST /api/feeds/:id/subscribe`；
    DELETE 翻转为退订 + 最后订阅者 prune（404 统一结构化 `json_error`，不泄露他人成员）。
    scheduler due-feed SELECT 加 `EXISTS(subscriptions)` 门；`feed.rs::fetch_feed` 执行门 ×2
    （feed 查找并入订阅 EXISTS、落库前复查，0 订阅 → `Ok(0)` 跳过）。lib.rs 移除三条 501
    死臂、CORS 重宣告 `X-User-Id`。
  - **frontend**：api.ts `getDeviceKey`（localStorage `rss_device_key` + `crypto.randomUUID`
    回退 + try/catch）+ 每请求带 `X-User-Id`；`getFeeds()` 钉死 **Discover-only**、新
    `getMyFeeds()` 供导航、`subscribeFeed`；`addFeed`→`AddFeedResult`、`deleteFeed`→
    `DeleteResult{id,pruned}`。app.ts 导航读 my 列表、池目录渲染 Discover「+」条（点才订阅，
    列表恒空也不自动加）；统计改为**本设备** active/failed/articles（`article_count` 求和），
    不再拿池全局计数当本设备数字；✕ 退订文案不承诺清池、toast 按 `pruned` 分流；空态文案
    引导 Discover。
  - **scripts/docs**：functional 测试加两条只读断言（OPTIONS 预检 `Allow-Headers` 含
    `X-User-Id`；`GET /api/me/feeds` 固定 key → success+数组、匿名 → 400）；契约脚本零
    功能改动仅注释（`/api/feeds` 仍全池、Discover-only，feeds==D1 等式不变）；TESTING.md
    期望数 71→73、§2 加新检查、§6 「feeds=3」改动态比对 + WS7 订阅门说明；
    PRODUCTION_BASELINE.md supersede 三段过时的「DELETE stub」行 + WS7 验收快照块（Gate 2
    回填）；public/index.html 补新端点 + `X-User-Id` 语义注。
  - **模型语义**：`X-User-Id` = 设备命名空间 id（≠鉴权 ≠授权，碰撞极低 ≠ 不可伪造），
    措辞在 ARCHITECTURE/`identity.rs`/migration 注释统一；`/api/feeds` Discover-only /
    `/api/me/feeds` navigation-only 不变式、池状态机（0 订阅 dormant / ≥1 active）、双门抓取
    均写入 §3.2/§5。
  - **Gate 2（部署验收，2026-09-06 完成，证据详见 PRODUCTION_BASELINE.md「WS7 设备模型」）**：
    prod 按评审 #9 顺序上线（apply 007 → worker 部署 → 紧邻 Pages → 立即真实浏览器订阅保留
    源 → 等 cron → 契约脚本）。证据构成：**API 层 19/19 PASS**（匿名 400、两全新 key 空起步、
    订 HN `created:true`、订池内 BBC `created:false` 收敛、退订共享 BBC `pruned:false`、唯一
    订阅者退订 HN `pruned:true` + 池恢复、重订 `created:false` 收敛）；**真实浏览器主设备**
    My Feeds = 本设备订阅集，池内他设备订阅的源不出现；**契约脚本** feeds==D1 + `<48h`
    ALL PASSED。全新第二设备的空起步 / 互不影响由 API 双 key + 真浏览器新窗口确认（操作者
    接受，未另跑 prod 别名隐身窗口）。验收时 Discover 池源数 = 12（见 PRODUCTION_BASELINE）。

- [x] **WS7.1 Discover 推荐目录（2026-09-06）**：给新设备一个 curated on-ramp ——
  **内置静态推荐目录**，worker 0 changes（无新表 / 无新 API / 不自动订阅 / 不建第二套 Feed
  模型）。Discover = **Recommended Catalog（目录段）+ Shared Pool（池尾段）两个独立语义段**，
  前端分别 render、绝不合并 —— 目录是系统精选，池尾只是「池里有、可订」的共享残留，杜绝
  「用户自贴的怪源被系统当成推荐源」。commit A `5ec3819`（证据层，test）→ B（feat，
  本记录所在 commit）。
  - **目录 = 产品配置**：`frontend/src/lib/recommended-feeds.ts`
    （`{name,url,category,description,tier:A|B|C}` 静态元数据，零 import、零浏览器 API）。
    移除推荐 = 删行（永不动 feeds/articles/subscriptions）；源死掉 = 重跑 harness → 更新
    evidence → 再改目录。19 源 = 4 锚点（BBC/OpenAI/V2EX/Guardian，prod-live snapshot）+
    15 edge 验证 GREEN。
  - **证据 ≠ 产品**：`scripts/ws7-1-verified.json` 只校验目录；RED/AMBER 行**永远保留**
    （「为什么 IEEE/arXiv/Reuters 不在」的答案）。判死理由（2026-09-06 edge）：IEEE/SAE/JPL/
    机器之心返回 HTML、Green Car Congress 530、Reuters 404、BleepingComputer 403、
    arXiv RSS 在 `rss.arxiv.org` 与 canonical `export.arxiv.org` 均为空文档。
  - **验证**：`scripts/ws7-1-validate-feeds.mjs`（maintenance tool，非日常 CI：每次跑会给
    dev 池瞬态增删自建 feed，并 REFUSE production host）—— 用 dev worker 真抓取判
    GREEN/AMBER/RED（HTTP + parse + article contract 含 canonical `published_at`）；
    `scripts/ws7-1-catalog-drill.mjs`（dev 回归，静态 gate「每行 GREEN」+ API round-trip，
    见 TESTING.md §2）。
  - **匹配规则（前端不另造 normalizer）**：已订判定仅
    `feed.url === 目录.url || feed.normalized_url === 目录.url` → Discover 行转 ✓
    （非交互、留原类别）；未订显示 `+`。**两个入口不统一**：目录行 `+` →
    `addFeed(url,name)`（find-or-create + 订到本设备）；池尾行 `+` → `subscribeFeed(id)`。
  - **编辑决策**：Google AI Blog 契约全绿但 feed 混招聘帖 → tier C（niche）；
    Mozilla Hacks 低产但活 → C；arXiv 两个官方宿主均空 → 不入。Security 空、Engineering
    仅 NASA —— 宁缺毋滥，要补类另开一轮 edge 验证再入目录（**不把非 GREEN 或已判 AMBER
    的源留在定稿目录**）。
  - **上线（2026-09-06）**：Pages production @ commit `1eb1257`（deploy `12b2c729`，branch
    master）；**纯前端，worker 未重部署**（0 改动）。真实浏览器 smoke（操作者接受）：主设备
    已订目录源 ✓（恰为其订阅集 ∩ 目录）、未订 +；My Feeds 不含池内他设备订阅源；全新设备
    首见 Recommended 19 全 +、池尾段仅露「非目录且未订」的池源。遗留外观项（pre-existing，
    非 WS7.1 引入）见 §8。

- [x] **WS7.2 Release Cleanup（2026-09-06）**：公开前**只清障、不重构**（评审结论：
  停在这里，不塞 AI，不做架构大修）。crate 名 `RSS` → `rss`（消 `non_snake_case`）；
  `cargo clippy --all-targets --all-features -- -D warnings` 从 28 → **0**。dead code 按
  「真死删除 / 测试专用 `cfg(test)`」分类：删 `FeedParser::fetch_feed`（生产走自由函数
  `fetch_feed`，此 assoc fn 连测试也无引用）、删 `types::CreateFeedRequest`（create
  handler 以 `serde_json::Value` 读 body，DTO 无任何引用 —— native 测试数 73→72）；
  `FeedParser::parse_rss/parse_atom`（测试按格式声明意图的命名入口，同一 `parse_document`）
  与 `queue::classify_run`（SQL 分类 CASE 的测试态规格镜像）改 `#[cfg(test)]`，不随 Worker
  发布。scheduler `#[event(scheduled)]` 的 `Result` 在 worker-rs 0.8.5 宏里被**静默丢弃**
  （`unused_must_use`，错误无感知）—— handler 改返回 `()`，body 挪进 `run_schedule` +
  `console_error` 记日志，顺带修掉该洞。Gate：native 72 tests / 0 warning、clippy
  `-D warnings` 0、wasm `cargo check`(+`--tests`) 干净、前端 typecheck+build 通过（本记录
  所在 commit）。

### 7.1 默认源一次性 bootstrap（006，非 reconcile）

`migrations/006_default_feeds.sql` 幂等地种入当前 3 个健康源（NYT World / BBC News /
OpenAI News，`fetch_interval_minutes=15`、`enabled=1`、`next_fetch_at=NULL`）。三条语义契约：

1. **one-time bootstrap，不是 reconcile**：`WHERE NOT EXISTS` 按 `normalized_url` 守卫，
   只对“缺该源”的库补种；用户删除的源不会因重复执行 migration 自动复活。
2. **`next_fetch_at = NULL` 依赖 scheduler 既有 NULL-as-due**（§5 选源条件
   `enabled=1 AND (next_fetch_at IS NULL OR …)`），种下后首个 cron 即抓取。
3. 对当前生产是 no-op（3 源已存在）。**空库自动加源只发生在后端/migration，前端不参与**。

## 8. 待办 / 后续

- [x] （方向已定为 feeds 即产品，2026-09-06）`rss_sources` / `rss_articles` dormant 层
      **去向已授权落地（WS5，同日，见 §7 WS5）**：code retired / data dormant ——
      运行期读写代码退休、`/api/sources` 收口成 501 retired API，表与数据**逐字节保留**
      （不 drop、不迁移），diagnostics 保留 dormant 计数。无遗留后续项（若未来要清表/
      删列仍属破坏性决定，须单独授权）。
- [ ] 数据迁移脚本参数化 DB id 后入库（当前读 `.env`）。
- [ ] 模块拆分（api/fetcher/parser/persistence）为可选重构，不阻塞业务。
- [ ] CI：worker deploy + pages deploy workflow 固化（当前 `ci.yml` 仅含质量门禁
      —— Rust test/clippy `-D warnings`/wasm + 前端 typecheck/build；部署段见 SETUP.md
      示例，需 CF secrets，未随库提供）。
- [ ] 前端相对时间对 **naive-UTC scheduler 时间戳**（`scheduler.last_run.started_at` /
      `last_fetch_run.started_at` 等，形如 `"2026-09-06 12:15:22"`，无 `Z`）按本机时区解析：
      `timeAgo`/`new Date` 在非 UTC 设备把「X 分钟前」偏成 +8h（UTC+8 实测 sync / feed `last`
      行偏移约 8h）。**pre-existing，WS7.1 未引入**（文章时间已是 canonical-ISO，无此问题）。
      修法候选：worker 输出侧给这类时间补 `Z`（naive 值即 UTC 墙上时间），或前端对无时区
      时间戳显式按 UTC 解析。

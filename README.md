# RSS Intelligence

Cloudflare RSS 聚合器，两件套：

- **Worker**（Rust → wasm32，worker-rs）：抓取 / 解析 / 持久化 / 调度。
- **Frontend**（Astro 静态站，生产在 rss-intelligence.pages.dev）：Worker API 的唯一客户端。

数据模型与设计决策见 [ARCHITECTURE.md](ARCHITECTURE.md)；环境 / 配置 / 部署见
[SETUP.md](SETUP.md)；测试矩阵见 [TESTING.md](TESTING.md)；生产只读验收基线见
[PRODUCTION_BASELINE.md](PRODUCTION_BASELINE.md)。

## 概念（feeds 即产品）

- **共享池 + 设备订阅**：`feeds` / `articles` 是**一份共享池**（Discover 的数据面）。
  `subscriptions (device_id, feed_id)` 把**每台设备各自的列表**挂到池上；`profiles` 把匿名
  设备 key（`X-User-Id` 头，浏览器 `localStorage` 的 UUID）映射为 INTEGER id。
  **0 订阅 = dormant**，调度器不抓；最后订阅者退订才把 feed + articles 一并 prune。
- **设备模型（WS7）**：新设备从**空列表**起步，靠 Discover 订阅建议或贴 URL 回源。
  `X-User-Id` 是设备命名空间 id —— **非鉴权、非账户**（碰撞极低但可伪造，不做安全边界）。
- **Discover（WS7.1）** = **Recommended 目录**（静态精选 19 源，GREEN-only，配置在
  `frontend/src/lib/recommended-feeds.ts`）+ **Shared Pool 尾段**（池里其余可订源）。
  两段独立渲染、**绝不合并**——目录是系统精选，尾段只是共享残留。
- **canonical `published_at`**：每行存储的 `published_at` 是固定 20 字符
  `YYYY-MM-DDTHH:MM:SSZ`，字符串排序 ≡ 时间排序。

## 目录结构

```
├─ src/                      # Rust worker（wasm32）
│  ├─ lib.rs                 # 入口 + 路由 + CORS（X-User-Id 重宣告）
│  ├─ routes.rs / db.rs      # API handler / D1 访问
│  ├─ feed.rs                # RSS/Atom 解析 + 抓取持久化管线（canonical published_at 收口）
│  ├─ identity.rs            # 设备 key 规范化 + profiles 查/插
│  ├─ queue.rs / scheduler.rs# cron 到期入队 → 队列消费抓取
│  ├─ types.rs / utils.rs    # serde 模型 / 时间 URL 工具
├─ migrations/               # 001–007 D1 schema（见下）
├─ scripts/                  # 维护/验证/演练：harness、drill、契约、回填、render-config
├─ public/index.html         # 静态 landing + API 速查（部署到 Pages）
├─ frontend/                 # Astro 站（rss-intelligence）
│  └─ src/{pages,components,lib,scripts,styles}
├─ wrangler.toml             # worker 配置（由 scripts/render-config.ps1 从 template 渲染）
├─ ARCHITECTURE.md / SETUP.md / TESTING.md / PRODUCTION_BASELINE.md / LICENSE
```

## Worker API

动态端点一律回 `Cache-Control: no-store` 并允许 `X-User-Id` 预检；带 `X-User-Id` 的端点在
匿名时答结构化 400 / 404。

| 端点 | 语义 |
|---|---|
| `GET /api/health` | 健康 + 全池计数 + `newest_published_at`（<48h 契约） |
| `GET /api/diagnostics` | 调度 / feed 健康 / cron_ticks / 抓取运行观测 |
| `GET /api/feeds` | **共享池目录（Discover-only）**，非设备视图 |
| `POST /api/feeds` | find-or-create + 订到本设备。body `{url,title}` → `{feed, created, already}` |
| `GET /api/me/feeds` | **本设备**列表（navigation-only；需 `X-User-Id`，匿名 400） |
| `POST /api/feeds/:id/subscribe` | 本设备订阅池内源（Discover「+」） |
| `POST /api/feeds/:id/fetch` | 单源立即抓取（fetch → parse → persist） |
| `GET /api/feeds/:id/articles` | 该源文章（50 条窗口，DESC） |
| `DELETE /api/feeds/:id` | **退订本设备**；最后订阅者 → 连池 prune。→ `{id, pruned}` |
| `/api/sources`（全 method） | **retired 501**：dormant `rss_sources` 原型层不可达（见 ARCHITECTURE §3） |

legacy 兼容：`GET /health`、`GET /feed`、`GET /feed/:id` 保留不回退（新代码一律走 `/api/*`）。

## 迁移（001–007）

| 迁移 | 内容 |
|---|---|
| 001_init | `feeds` / `articles` 基表 |
| 002_add_error_message | `feeds.error_message` |
| 003_cron_ticks | cron 心跳观测表 |
| 004_rss_sources | dormant 原型层（`rss_sources`/`rss_articles`，**逐字节保留**，代码已退休） |
| 005_feed_health_fetch_runs | feed 健康 + `fetch_runs` 运行观测 |
| 006_default_feeds | 一次性空库种入默认源（幂等、非 reconcile） |
| 007_device_profiles | `profiles`（设备命名空间注册表）+ `subscriptions.user_id` 引用 |

## 开发 / 测试 / 部署

全部环境配置（`.env` 模板、wrangler secrets、CF ID、部署命令）在 [SETUP.md](SETUP.md)，
README 不重复。测试矩阵（单测 / functional / drills / perf / 契约 / 前端 build）在
[TESTING.md](TESTING.md)。一句话：

```bash
cargo test --all                       # 72 native 单测
pwsh scripts/test-functional.ps1       # 只读 functional（dev/prod URL）
node scripts/ws7-device-drill.mjs      # 设备隔离演练（dev-only，refuse prod）
node scripts/check-articles-contract.mjs  # 生产只读契约（feeds==D1、published_at canonical、<48h）
cd frontend && npm run build           # Astro 构建 → wrangler pages deploy dist
```

## 许可证

[MIT](LICENSE) © 2026 weixc0856。

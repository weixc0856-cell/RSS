# 🔧 环境配置指南

本项目使用环境变量管理敏感信息和配置。所有凭证和 ID 都应该存储在环境中，**不要**提交到 Git。

## 文件说明

| 文件 | 用途 | Git 追踪 | 敏感信息 |
|------|------|---------|--------|
| `.env.example` | 环境变量模板（示例）| ✅ 是 | ❌ 否 |
| `.env.local` | 本地开发配置 | ❌ 否 | ✅ 是 |
| `.env.production.example` | 生产配置模板 | ✅ 是 | ❌ 否 |
| `.env.production` | 生产环境配置 | ❌ 否 | ✅ 是 |
| `wrangler.toml` | Wrangler 配置（由 `scripts/render-config.ps1` 渲染生成） | ❌ 否 | ✅ 是 |
| `wrangler.toml.template` | Wrangler 配置模板（占位符） | ✅ 是 | ❌ 否 |
| `scripts/render-config.ps1` | 渲染脚本（占位符 → 真实 ID） | ✅ 是 | ❌ 否 |

## ⚠️ 安全第一：绝不在代码中提交真实 ID

提交到 Git 的 `wrangler.toml.template` 和 `.env.example` 中只使用**占位符**，真实 ID 存储在本地 `.env.local` 和 `.env.production` 中，再由渲染脚本生成真实的 `wrangler.toml`（已被 `.gitignore` 忽略）。

```yaml
# ✅ 好的做法 - wrangler.toml.template（可提交）
database_id = "{{PROD_D1_DATABASE_ID}}"

# ❌ 错误 - 不要这样做
database_id = "6d199e6f-87a7-4177-b7bb-862d72f61797"  # 真实ID泄露！
```

## 🔄 渲染 wrangler.toml

每次修改了环境文件或资源 ID 后，先运行渲染脚本生成 `wrangler.toml`，再执行 wrangler 命令：

```bash
pwsh scripts/render-config.ps1            # 读取 .env.local + .env.production
```

取值优先级：**进程环境变量 > `.env.production` > `.env.local`**。若模板中的占位符（如 `{{D1_DATABASE_ID}}`）缺失，脚本会报错并列出缺失项。CI 中可直接用 GitHub Secrets / `env:` 导出变量后再渲染。

## 快速开始

### 1️⃣ 开发环境配置

```bash
# 复制开发环境模板
cp .env.example .env.local

# 编辑 .env.local 填入开发环境的实际 ID

# 渲染生成 wrangler.toml（把占位符替换为真实 ID）
pwsh scripts/render-config.ps1

# 本地开发（wrangler 会自动读取 .env.local）
wrangler dev
```

### 2️⃣ 生产环境配置

```bash
# 复制生产环境模板
cp .env.production.example .env.production

# 编辑 .env.production 填入生产环境的实际 ID
# 不要提交 .env.production 到 Git！

# 重新渲染 wrangler.toml（让生产 ID 生效）
pwsh scripts/render-config.ps1
```

### 3️⃣ 部署

```bash
# 本地部署到生产（需要 .env.production）
wrangler deploy --env production

# 或通过 GitHub Actions 使用 Secrets 部署
```

## 获取 Cloudflare IDs

#### KV Namespace ID
```bash
wrangler kv:namespace list
# 或创建新的
wrangler kv:namespace create CACHE
```

#### D1 Database ID
```bash
wrangler d1 list
# 或创建新的
wrangler d1 create rss-db
```

#### R2 Bucket
```bash
wrangler r2 bucket list
# 或创建新的
wrangler r2 bucket create rss-bucket
```

#### Account ID
```bash
wrangler whoami
```

## 使用环境

### 本地开发（开发环境）

```bash
# 确保已渲染 wrangler.toml（含真实 ID）
pwsh scripts/render-config.ps1

# 自动使用 .env.local 和开发配置
wrangler dev
```

### 部署到生产环境

```bash
# 重新渲染（读取 .env.local + .env.production 的生产 ID）
pwsh scripts/render-config.ps1

# 使用生产环境配置
wrangler deploy --env production
```

## 敏感信息管理

### 使用 Wrangler Secrets（推荐）

对于敏感信息（API 密钥、令牌等），使用 Wrangler Secrets：

```bash
# 设置开发环境 secrets
wrangler secret put API_KEY --env development

# 设置生产环境 secrets
wrangler secret put API_KEY --env production

# 查看已设置的 secrets
wrangler secret list
```

在代码中访问：
```rust
let api_key = env.secret("API_KEY")?;
```

### 访问环境变量

在 Rust 代码中：
```rust
let cache_ttl = env.var("CACHE_TTL_SECONDS")?;
let log_level = env.var("LOG_LEVEL")?;
```

## 安全最佳实践

✅ **务必做到：**
- ✅ 将 `.env.local` 添加到 `.gitignore`（已配置）
- ✅ 用 `.env.example` 作为模板
- ✅ 对敏感信息使用 `wrangler secret`
- ✅ 定期轮换密钥和令牌
- ✅ 使用环境特定的配置（dev vs prod）

❌ **务必避免：**
- ❌ 在 `.env` 或代码中提交凭证
- ❌ 在公开库中存储生产 IDs
- ❌ 在版本控制中提交实际的 API 密钥
- ❌ 在代码中硬编码敏感信息

## CI/CD 集成

### GitHub Actions（推荐生产部署方式）

在 GitHub 中安全地存储敏感信息：

1. **设置 GitHub Secrets**
   - 进入 Repository > Settings > Secrets and variables > Actions
   - 添加以下 Secrets：

| Secret 名称 | 值 |
|-----------|---|
| `CLOUDFLARE_API_TOKEN` | 从 Cloudflare 获取 |
| `CLOUDFLARE_ACCOUNT_ID` | 账户 ID |
| `D1_DATABASE_ID` | 开发 D1 数据库 ID |
| `KV_NAMESPACE_ID` | 开发 KV Namespace ID |
| `PROD_D1_DATABASE_ID` | 生产 D1 数据库 ID |
| `PROD_KV_NAMESPACE_ID` | 生产 KV Namespace ID |
| `R2_BUCKET_NAME` | 开发 R2 桶名 |
| `PROD_R2_BUCKET_NAME` | 生产 R2 桶名 |
| `RSS_FETCH_QUEUE` | 队列名 |
| `RSS_FETCH_DLQ` | 死信队列名 |

2. **创建部署工作流**

```yaml
# .github/workflows/deploy-production.yml
name: Deploy to Production

on:
  push:
    branches: [main]

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Setup Rust
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      
      - name: Install wrangler
        run: npm install -g wrangler

      # 用 GitHub Secrets 渲染出真实的 wrangler.toml（占位符不入库）
      - name: Render wrangler config
        run: pwsh scripts/render-config.ps1
        env:
          D1_DATABASE_ID: ${{ secrets.D1_DATABASE_ID }}
          KV_NAMESPACE_ID: ${{ secrets.KV_NAMESPACE_ID }}
          PROD_D1_DATABASE_ID: ${{ secrets.PROD_D1_DATABASE_ID }}
          PROD_KV_NAMESPACE_ID: ${{ secrets.PROD_KV_NAMESPACE_ID }}
          R2_BUCKET_NAME: ${{ secrets.R2_BUCKET_NAME }}
          PROD_R2_BUCKET_NAME: ${{ secrets.PROD_R2_BUCKET_NAME }}
          RSS_FETCH_QUEUE: ${{ secrets.RSS_FETCH_QUEUE }}
          RSS_FETCH_DLQ: ${{ secrets.RSS_FETCH_DLQ }}
      
      - name: Deploy to Production
        run: wrangler deploy --env production
        env:
          CLOUDFLARE_API_TOKEN: ${{ secrets.CLOUDFLARE_API_TOKEN }}
          CLOUDFLARE_ACCOUNT_ID: ${{ secrets.CLOUDFLARE_ACCOUNT_ID }}
```

3. **本地部署（仅用于测试）**

```bash
# 先渲染 wrangler.toml（读取 .env.local + .env.production 的真实 ID）
pwsh scripts/render-config.ps1

# 再部署生产环境
wrangler deploy --env production
```

**⚠️ 重要：** 绝不要在本地提交 `.env.production` 到 GitHub，CI/CD 使用 GitHub Secrets 自动处理。

## 环境变量参考

### Cloudflare 资源

| 变量 | 说明 | 获取方式 |
|------|------|---------|
| `KV_NAMESPACE_ID` | 开发 KV 存储命名空间 ID | `wrangler kv namespace list` |
| `PROD_KV_NAMESPACE_ID` | 生产 KV 存储命名空间 ID | `wrangler kv namespace list` |
| `D1_DATABASE_ID` | 开发 D1 数据库 ID | `wrangler d1 list` |
| `PROD_D1_DATABASE_ID` | 生产 D1 数据库 ID | `wrangler d1 list` |
| `R2_BUCKET_NAME` | 开发 R2 存储桶名称 | `wrangler r2 bucket list` |
| `PROD_R2_BUCKET_NAME` | 生产 R2 存储桶名称 | `wrangler r2 bucket list` |
| `R2_ACCOUNT_ID` | Cloudflare 账户 ID | `wrangler whoami` |
| `RSS_FETCH_QUEUE` | 队列名 | `wrangler queues list` |
| `RSS_FETCH_DLQ` | 死信队列名 | `wrangler queues list` |

### 应用配置

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `ENVIRONMENT` | development | dev / production |
| `LOG_LEVEL` | debug | debug / info / warn / error |
| `API_RATE_LIMIT` | 100 | 每分钟请求数 |
| `CACHE_TTL_SECONDS` | 3600 | 缓存过期时间（秒）|
| `MAX_FEED_ITEMS` | 50 | 每个源的最大项目数 |
| `FEED_FETCH_INTERVAL_MINUTES` | 30 | Feed 更新间隔（分钟）|

## 故障排除

### "database_id not found"
- 检查 `D1_DATABASE_ID` 是否正确
- 运行 `wrangler d1 list` 确认

### KV 无法连接
- 验证 `KV_NAMESPACE_ID` 正确
- 运行 `wrangler kv:namespace list` 检查

### 环境变量未被读取
- 确保变量在 `wrangler.toml.template` 的正确 `[env]` 部分
- 检查 `.env.local` 是否存在（本地开发）
- 使用 `wrangler dev` 时自动读取 `.env.local`
- 修改 `.env.*` 后记得先运行 `pwsh scripts/render-config.ps1` 重新生成 `wrangler.toml`

## 更新日志

| 日期 | 变更 |
|------|------|
| 2026-09-02 | 初始环境配置 |
| 2026-09-02 | 真实创建全部 Cloudflare 资源；引入 `scripts/render-config.ps1`，真实 ID 改为渲染生成且不入库（模板 `wrangler.toml.template`） |

---

📚 参考资源：
- [Wrangler 文档](https://developers.cloudflare.com/workers/wrangler/)
- [Cloudflare Secrets](https://developers.cloudflare.com/workers/platform/environment-variables/#secrets)
- [D1 文档](https://developers.cloudflare.com/d1/)
- [KV 文档](https://developers.cloudflare.com/workers/runtime-apis/kv/)

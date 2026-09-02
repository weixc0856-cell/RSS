# 🔗 RSS Worker

A powerful Cloudflare Workers RSS feed aggregator and parser built with Rust, Wrangler, and Workers D1 database.

## Features

- 📡 **Feed Management** — Add, update, and remove RSS/Atom feeds
- 🔍 **Feed Parsing** — Support for RSS 2.0 and Atom feed formats
- 💾 **Database Storage** — Persistent storage with Cloudflare D1
- ⚡ **Caching** — KV store for performance optimization
- 🏥 **Health Checks** — Built-in health endpoint
- 🔐 **Secure** — Runs on Cloudflare's edge network
- 📊 **Scalable** — Auto-scaling without infrastructure management

## Project Structure

```
D:\Project\RSS\
├─ Cargo.toml              # Rust dependencies and config
├─ wrangler.toml           # Cloudflare Workers configuration
├─ package.json            # NPM configuration
├─ README.md               # This file
├─ migrations/
│  └─ 001_init.sql         # Database initialization
├─ src/
│  ├─ lib.rs               # Main worker entry point (fetch router)
│  ├─ types.rs             # Data models and types
│  ├─ routes.rs            # API route handlers (incl. GET /api/diagnostics)
│  ├─ db.rs                # D1 access
│  ├─ feed.rs              # RSS/Atom feed parsing & fetch/persist pipeline
│  ├─ queue.rs             # Queue consumer (fetch jobs produced by scheduler)
│  └─ scheduler.rs         # Cron trigger: enqueues due feeds (hourly)
├─ public/
│  └─ index.html           # Landing page with API docs
└─ build/
   └─ worker/
      └─ shim.mjs          # Worker entry point
```

## API Endpoints

### Health Check
- `GET /health` — Returns `ok` if the service is running

### Feed Management
- `GET /api/feeds` — Get all subscribed feeds
- `POST /api/feeds` — Create a new feed
  - Request body: `{ "url": "https://...", "title": "..." }`
- `GET /api/feeds/:feed_id/items` — Get feed items
- `DELETE /api/feeds/:feed_id` — Delete a feed

## Setup

### Prerequisites
- [Rust](https://www.rust-lang.org/tools/install) 1.56+
- [Wrangler CLI](https://developers.cloudflare.com/workers/wrangler/install-and-update/)
- Cloudflare account with Workers enabled
- D1 database created in Cloudflare

### Installation

1. Clone the repository
   ```bash
   git clone <repository-url>
   cd D:\Project\RSS
   ```

2. Install dependencies
   ```bash
   npm install
   ```

3. Build the project
   ```bash
   wrangler build
   ```

4. Set up the database
   ```bash
   wrangler d1 execute <database-name> < migrations/001_init.sql
   ```

## Development

### Local Testing
```bash
wrangler dev
```

The worker will be available at `http://localhost:8787`

### Build
```bash
wrangler build
```

### Deploy
```bash
wrangler deploy
```

## Testing

The core logic — RSS/Atom feed parsing, utilities, and data models — is
covered by native Rust unit tests:

```bash
cargo test
```

Coverage by module:

- `src/feed.rs` — RSS 2.0 / Atom parsing, entity + CDATA handling, GUID
  fallback, namespace-prefixed tags, malformed XML errors, and article hash
  generation.
- `src/types.rs` — serde serialization/deserialization round-trips for every
  model and request/response payload.
- `src/utils.rs` — ID generation format, RFC 3339 timestamps, URL validation.

> **Note:** Handlers that depend on the Cloudflare Worker runtime
> (`worker::Request` / `Response`, D1, KV, fetch) execute inside a
> WebAssembly sandbox, so they cannot be exercised by host `cargo test`.
> They should be validated with `wrangler dev` and HTTP requests against the
> running worker.

## Dependencies

| Package | Version | Purpose |
|---------|---------|---------|
| `worker` | 0.8.5 | Cloudflare Workers SDK |
| `serde` | 1.0 | Serialization/deserialization |
| `serde_json` | 1.0 | JSON handling |
| `chrono` | 0.4 | Date/time operations |
| `uuid` | 1.0 | ID generation |
| `tokio` | 1.0 | Async runtime |
| `reqwest` | 0.11 | HTTP requests |

## Database Schema

### feeds
- `id` (TEXT PRIMARY KEY) — Unique feed identifier
- `url` (TEXT NOT NULL UNIQUE) — Feed source URL
- `title` (TEXT NOT NULL) — Feed title
- `description` (TEXT) — Feed description
- `created_at` (TEXT NOT NULL) — Creation timestamp
- `updated_at` (TEXT NOT NULL) — Last update timestamp

### feed_items
- `id` (TEXT PRIMARY KEY) — Unique item identifier
- `feed_id` (TEXT NOT NULL FK) — Reference to parent feed
- `title` (TEXT NOT NULL) — Item title
- `description` (TEXT) — Item description/content
- `link` (TEXT NOT NULL) — Item source link
- `published_at` (TEXT) — Publication timestamp
- `created_at` (TEXT NOT NULL) — Creation timestamp

## Performance Optimization

- **KV Cache** — Feed parsing results cached in Cloudflare KV
- **Database Indexing** — Optimized queries with indexes on `feed_id` and `published_at`
- **Edge Execution** — Content delivered from edge locations globally

## Security

- Input validation for all URLs
- SQL injection prevention through prepared statements
- Rate limiting via Cloudflare
- CORS headers configured appropriately
- Environment variables for sensitive data

## Troubleshooting

### Build fails with "cannot find package `worker`"
```bash
cargo update
cargo build
```

### Database connection fails
- Verify D1 database is linked in `wrangler.toml`
- Check database name matches
- Run migrations: `wrangler d1 execute <db-name> < migrations/001_init.sql`

### Feed parsing returns empty results
- Verify feed URL is accessible
- Check feed format (RSS 2.0 or Atom)
- Review logs: `wrangler tail`

## Contributing

1. Create a feature branch
2. Make your changes
3. Test locally with `wrangler dev`
4. Deploy to staging
5. Submit a pull request

## License

MIT

## Support

For issues and questions:
- 📖 [Cloudflare Workers Documentation](https://developers.cloudflare.com/workers/)
- 🦀 [Rust Documentation](https://doc.rust-lang.org/)
- 📡 [RSS Standard](https://www.rssboard.org/)
- 🔗 [Atom Syndication Format](https://tools.ietf.org/html/rfc4287)

---

Built with ❤️ on Cloudflare Workers

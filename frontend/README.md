# RSS Intelligence (Astro frontend)

Astro 5 single-page reader for the Cloudflare Worker RSS API.

## Separation of concerns

- `src/pages|layouts|components/` — HTML structure (`.astro`)
- `src/styles/` — CSS only (tokens / base / layout / components)
- `src/scripts/` — behaviour only (`app.ts` state+render, `animate.ts` AOS wrapper)
- `src/lib/` — data & API layer (`types.ts`, `api.ts`); the only place that calls
  the Worker over HTTP/JSON

The UI never talks to the backend directly: every request goes through
`src/lib/api.ts`.

## Dev

```bash
npm install
npm run dev          # http://localhost:4321
```

## Env

One Worker API base per build: `ASTRO_PUBLIC_API_BASE` (Vite public env,
compile-time). Unset, it defaults to the production Worker
(`https://rss-worker-production.weixc0856.workers.dev`) — see `src/lib/api.ts`.
To point a build elsewhere, copy `.env.example` to `.env` and uncomment the
variable.

Environment is a **deployment** concern, not a per-user UI switch: there is no
Dev/Prod toggle in the UI and nothing is persisted for it. Every key read from
`import.meta.env` is declared in `src/env.d.ts`.

## Build

```bash
npm run build        # static output in dist/
npm run preview
```

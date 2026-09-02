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

Defaults point at the live Workers. To override at build time copy
`.env.example` to `.env` and uncomment:

- `ASTRO_PUBLIC_API_DEV` — dev Worker
- `ASTRO_PUBLIC_API_PROD` — prod Worker

You can also switch Dev/Prod inside the UI (persisted in localStorage).

## Build

```bash
npm run build        # static output in dist/
npm run preview
```

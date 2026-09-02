/// <reference types="astro/client" />

interface ImportMetaEnv {
  /** Dev API base (defaults to the rss-worker workers.dev URL). */
  readonly ASTRO_PUBLIC_API_DEV?: string;
  /** Production API base (defaults to rss-worker-production). */
  readonly ASTRO_PUBLIC_API_PROD?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

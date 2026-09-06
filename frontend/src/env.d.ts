/// <reference types="astro/client" />

interface ImportMetaEnv {
  /** Worker API base — build-time, deployment-level override only (no runtime/UI
   *  switch). When unset, api.ts falls back to the production Worker URL. */
  readonly ASTRO_PUBLIC_API_BASE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

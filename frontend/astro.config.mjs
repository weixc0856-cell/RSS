import { defineConfig } from "astro/config";

// Static frontend; talks to the Cloudflare Worker purely over HTTP/JSON.
export default defineConfig({
  output: "static",
  site: "https://rss-intelligence.pages.dev",
});

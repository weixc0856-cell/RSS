/**
 * Recommended Discover catalog — PRODUCT SOURCE OF TRUTH (static).
 *
 * Invariants:
 *   - This file is the single source for the "Recommended" segment of Discover.
 *     It is static metadata ONLY: it never creates a feed and never subscribes.
 *     Clicking "+" runs POST /api/feeds (find-or-create + subscribe current device).
 *   - Every row MUST have a GREEN verdict for its exact url in
 *     scripts/ws7-1-verified.json (dev-edge evidence). Removing a
 *     recommendation = deleting a row here (config); re-verifying the catalog =
 *     scripts/ws7-1-validate-feeds.mjs (evidence). verified.json also keeps the
 *     RED/AMBER rows that never shipped, to answer "why isn't X recommended".
 *   - No imports, no browser APIs, no runtime logic — pure data, so the Node
 *     harness and catalog drill import it directly.
 *
 * Bootstrapped 2026-09-06 from a dev-edge run over 28 official-source
 * candidates: 19 GREEN ship (4 snapshotted from live production rows, 15
 * edge-validated). Excluded with evidence: IEEE Spectrum / NASA JPL / SAE /
 * 机器之心 return HTML pages, Green Car Congress 530, Reuters 404,
 * BleepingComputer 403, arXiv RSS empty at all official hosts. Tiers are
 * editorial (A flagship / B strong / C niche).
 */
export type RecommendedFeed = {
  name: string;
  url: string;
  category: string;
  description: string;
  tier: "A" | "B" | "C";
};

export const RECOMMENDED_FEEDS: RecommendedFeed[] = [
  // ── Technology ────────────────────────────────────────────────────────────
  {
    name: "Hacker News",
    url: "https://news.ycombinator.com/rss",
    category: "Technology",
    description: "Technology, startups, programming and AI discussions.",
    tier: "A",
  },
  {
    name: "MIT Technology Review",
    url: "https://www.technologyreview.com/feed/",
    category: "Technology",
    description: "Technology, AI, computing and emerging science.",
    tier: "A",
  },
  {
    name: "Ars Technica",
    url: "https://feeds.arstechnica.com/arstechnica/index/",
    category: "Technology",
    description: "In-depth technology news and analysis.",
    tier: "A",
  },
  {
    name: "The Register",
    url: "https://www.theregister.com/headlines.atom",
    category: "Technology",
    description: "IT, engineering, security and enterprise technology.",
    tier: "B",
  },
  // ── AI ────────────────────────────────────────────────────────────────────
  {
    name: "OpenAI News",
    url: "https://openai.com/news/rss.xml",
    category: "AI",
    description: "OpenAI news and announcements.",
    tier: "A",
  },
  {
    name: "Hugging Face Blog",
    url: "https://huggingface.co/blog/feed.xml",
    category: "AI",
    description: "Open-source AI, models and ML engineering.",
    tier: "A",
  },
  {
    name: "NVIDIA Technical Blog",
    url: "https://blogs.nvidia.com/feed/",
    category: "AI",
    description: "AI, GPU, HPC and accelerated computing.",
    tier: "B",
  },
  {
    name: "Google AI Blog",
    url: "https://blog.google/technology/ai/rss/",
    category: "AI",
    description: "AI research and product engineering (feed also carries careers posts).",
    tier: "C",
  },
  // ── Science & Research ────────────────────────────────────────────────────
  {
    name: "Nature",
    url: "https://www.nature.com/nature.rss",
    category: "Science & Research",
    description: "Flagship multidisciplinary science research.",
    tier: "A",
  },
  {
    name: "Science (AAAS)",
    url: "https://www.science.org/rss/news_current.xml",
    category: "Science & Research",
    description: "Latest science research and news.",
    tier: "A",
  },
  // ── Engineering ───────────────────────────────────────────────────────────
  {
    name: "NASA News",
    url: "https://www.nasa.gov/feed/",
    category: "Engineering",
    description: "NASA news releases and discoveries.",
    tier: "A",
  },
  // ── Automotive ────────────────────────────────────────────────────────────
  {
    name: "Automotive World",
    url: "https://www.automotiveworld.com/feed/",
    category: "Automotive",
    description: "OEMs, mobility and automotive industry.",
    tier: "B",
  },
  // ── China ─────────────────────────────────────────────────────────────────
  {
    name: "V2EX",
    url: "https://www.v2ex.com/feed/tab/all.xml",
    category: "China",
    description: "Tech community: programming, startups and geek talk.",
    tier: "A",
  },
  {
    name: "量子位",
    url: "https://www.qbitai.com/feed",
    category: "China",
    description: "AI industry and research (Chinese).",
    tier: "B",
  },
  {
    name: "少数派",
    url: "https://sspai.com/feed",
    category: "China",
    description: "Technology and productivity (Chinese).",
    tier: "B",
  },
  // ── World ─────────────────────────────────────────────────────────────────
  {
    name: "BBC News",
    url: "https://feeds.bbci.co.uk/news/rss.xml",
    category: "World",
    description: "Global news baseline.",
    tier: "A",
  },
  {
    name: "The Guardian",
    url: "https://www.theguardian.com/technology/rss",
    category: "World",
    description: "Independent journalism and technology.",
    tier: "A",
  },
  // ── Developer ─────────────────────────────────────────────────────────────
  {
    name: "GitHub Blog",
    url: "https://github.blog/feed/",
    category: "Developer",
    description: "Developer, open source and engineering.",
    tier: "B",
  },
  {
    name: "Mozilla Hacks",
    url: "https://hacks.mozilla.org/feed/",
    category: "Developer",
    description: "Web, browser and developer engineering.",
    tier: "C",
  },
];

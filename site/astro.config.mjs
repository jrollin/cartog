// @ts-check
import { defineConfig } from "astro/config";
import pagefind from "astro-pagefind";

// Deployed to GitHub Pages at https://jrollin.github.io/cartog/.
// build.format "file" keeps the original flat URLs (usage.html, not usage/),
// so the live links, install.sh URL, and relative asset paths stay identical
// to the pre-Astro static site.
export default defineConfig({
  site: "https://jrollin.github.io",
  base: "/cartog",
  // Preserve significant whitespace in <pre>/terminal code blocks: the default
  // HTML compressor collapses the newlines inside them into spaces.
  compressHTML: false,
  build: {
    format: "file",
    assets: "_astro",
  },
  // Pagefind builds a static search index from the built HTML at `astro build`
  // and serves the UI client-side (no server) — fits GitHub Pages. Only regions
  // tagged `data-pagefind-body` are indexed; the docs page (usage.astro) carries
  // it, so search is scoped to the docs.
  integrations: [pagefind()],
});

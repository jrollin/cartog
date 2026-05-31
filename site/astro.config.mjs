// @ts-check
import { defineConfig } from "astro/config";

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
});

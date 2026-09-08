// Post-build check for the machine-readable endpoints.
//
// These pages exist so an agent can read the site cheaply, and every failure
// mode here is silent: a rewritten link that kept a repo-relative target 404s,
// an endpoint that stopped emitting just vanishes, and a stale index advertises
// pages that no longer exist. `astro build` reports none of that, so assert it.
import fs from "node:fs";
import path from "node:path";

const DIST = path.resolve(import.meta.dirname, "..", "dist");
const failures = [];

function read(name) {
  const file = path.join(DIST, name);
  if (!fs.existsSync(file)) {
    failures.push(`${name}: not emitted by the build`);
    return null;
  }
  return fs.readFileSync(file, "utf8");
}

// 1. Every endpoint emitted, and non-trivial. A helper that throws mid-render
//    can still leave a short file behind.
// Floors set just under the current sizes (llms-full ~5.9k, llms ~1.1k,
// index.md ~32k, usage.md ~23k). A generous floor cannot detect the failure
// this check exists for: a helper that throws mid-render leaves a short file.
const MIN_BYTES = {
  "llms.txt": 900,
  "llms-full.txt": 5000,
  "index.md": 28000,
  "usage.md": 20000,
};
const bodies = {};
for (const [name, min] of Object.entries(MIN_BYTES)) {
  const body = read(name);
  if (body === null) continue;
  bodies[name] = body;
  if (body.length < min) {
    failures.push(`${name}: ${body.length} bytes, expected at least ${min}`);
  }
}

// 2. No relative link survived the rewrite. `docs/` is not published here, so a
//    surviving relative target is a 404 for every reader of the .md pages.
for (const name of ["index.md", "usage.md", "llms.txt", "llms-full.txt"]) {
  const body = bodies[name];
  if (!body) continue;
  // Markdown links whose target is neither absolute, an anchor, nor a mail link.
  for (const match of body.matchAll(/\[(?:[^[\]]|\[[^\]]*\])*\]\(([^)\s]+)\)/g)) {
    const href = match[1];
    if (!/^(https?:|mailto:|#)/.test(href)) {
      failures.push(`${name}: unrewritten relative link "${href}"`);
    }
  }
}

// 3. Every site page the index advertises actually exists in dist.
const index = bodies["llms-full.txt"];
if (index) {
  // Any advertised path, including one in a subdirectory or carrying an anchor.
  // The extension list bounds the match so a sentence-final period is not
  // swallowed into the filename.
  for (const match of index.matchAll(
    /https:\/\/www\.cartog\.dev\/([^\s)]*\.(?:html|md|txt|sh|gif))(#[A-Za-z0-9_-]+)?/g,
  )) {
    const [, asset, anchor] = match;
    const target = path.join(DIST, asset);
    // install.sh and demo.gif ship from public/, the rest are built pages.
    if (!fs.existsSync(target)) {
      failures.push(`llms-full.txt: advertises /${asset}, which is not in dist`);
      continue;
    }
    // A dangling anchor is as broken as a dangling page, and just as silent.
    if (anchor && asset.endsWith(".html")) {
      const id = anchor.slice(1);
      const html = fs.readFileSync(target, "utf8");
      if (!html.includes(`id="${id}"`)) {
        failures.push(`llms-full.txt: advertises /${asset}${anchor}, but no id="${id}" in that page`);
      }
    }
  }
}

if (failures.length > 0) {
  console.error("machine-readable page check FAILED:");
  for (const f of failures) console.error(`  - ${f}`);
  process.exit(1);
}
console.log(`machine-readable pages OK (${Object.keys(MIN_BYTES).join(", ")})`);

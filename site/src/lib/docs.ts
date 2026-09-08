// Reads the canonical Markdown in `docs/` at build time so the machine-readable
// endpoints (`/llms-full.txt`, `/*.md`) never become a second copy of any fact.
// A generated page that drifts from `docs/` is the failure this avoids: the repo
// rule is that docs and site must agree, and a hand-written Markdown twin would
// have made that three copies instead of two.
import fs from "node:fs";
import path from "node:path";

/** Repo root, two levels above `site/`. Resolved from cwd, which is `site/` during a build. */
const REPO_ROOT = path.resolve(process.cwd(), "..");
const DOCS_DIR = path.join(REPO_ROOT, "docs");

/** Read a repo-relative file. Throws at build time rather than emitting a page with a hole in it. */
export function readRepoFile(relPath: string): string {
  return fs.readFileSync(path.join(REPO_ROOT, relPath), "utf8");
}

/** One entry of the docs index, parsed out of `docs/README.md`'s bullet lists. */
export interface DocEntry {
  /** Diataxis group heading the bullet appeared under, e.g. "Reference". */
  group: string;
  /** Path relative to `docs/`, e.g. "reference/cli.md". */
  docPath: string;
  /** The one-line description after the em-dash, with Markdown links flattened. */
  summary: string;
}

/**
 * Parse `docs/README.md` into a flat list of entries.
 *
 * That file is already a curated index with a one-line description per doc, so
 * it is the source rather than a directory walk: a walk would list redirect
 * stubs and per-directory READMEs that carry no description, and would have no
 * ordering.
 */
export function docsIndex(): DocEntry[] {
  const src = fs.readFileSync(path.join(DOCS_DIR, "README.md"), "utf8");
  const entries: DocEntry[] = [];
  let group = "";

  for (const line of src.split("\n")) {
    const heading = line.match(/^##\s+(.+?)\s*$/);
    if (heading) {
      group = heading[1];
      continue;
    }
    // `- [path/to.md](path/to.md) — description`, em-dash or hyphen separator.
    const bullet = line.match(/^-\s+\[[^\]]+\]\(([^)]+)\)\s*[—-]\s*(.+?)\s*$/);
    if (!bullet) continue;

    const [, href, rawSummary] = bullet;
    // Skip links out of `docs/` (the root README) and any non-Markdown target.
    if (href.startsWith("../") || href.startsWith("http") || !href.endsWith(".md")) continue;

    entries.push({ group, docPath: href, summary: flattenInline(rawSummary) });
  }
  return entries;
}

/** Strip inline Markdown links and emphasis so a summary reads cleanly as one plain line. */
function flattenInline(text: string): string {
  return text
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .trim();
}

/** Absolute URL of a doc on GitHub, since `docs/` is not published as part of this site. */
export function docSourceUrl(docPath: string): string {
  return `https://github.com/jrollin/cartog/blob/main/docs/${docPath}`;
}

/** Options for [`markdownPage`]. */
export interface MarkdownPageOptions {
  /** Repo-relative Markdown file to serve, e.g. "docs/usage.md". */
  repoPath: string;
  /** Canonical HTML URL of the equivalent page, stated in the header. */
  canonical: string;
  /** Repo directory the file's relative links resolve against ("docs", or "" for the root). */
  linkBase: string;
}

const GITHUB_BLOB = "https://github.com/jrollin/cartog/blob/main";
/** Images must resolve to bytes: a `blob/` URL serves a GitHub HTML page. */
const GITHUB_RAW = "https://raw.githubusercontent.com/jrollin/cartog/main";

/**
 * Serve a canonical repo Markdown file as a `.md` page.
 *
 * Prepends a one-line header naming the canonical URL and the machine index, so
 * an agent that lands on the Markdown alone can still find both.
 */
export function markdownPage(opts: MarkdownPageOptions): Response {
  const body = rewriteRelativeLinks(readRepoFile(opts.repoPath), opts.linkBase);
  const header =
    `> cartog: a code graph indexer for LLM coding agents. ` +
    `Canonical: ${opts.canonical} · ` +
    `Full index: https://www.cartog.dev/llms-full.txt\n\n`;

  return new Response(header + body, {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
}

/**
 * Point relative Markdown links at their GitHub source.
 *
 * `docs/` is not published as part of this site, so a link like
 * `[troubleshooting.md](troubleshooting.md)` would 404 for anyone reading the
 * `.md` endpoint. Absolute URLs and bare anchors are left alone.
 */
function rewriteRelativeLinks(markdown: string, linkBase: string): string {
  // The label may itself contain a bracketed image (a shield badge wrapped in a
  // link), so the label pattern allows one nested `[...]` level; a plain
  // `[^\]]+` stops at the inner bracket and leaves the outer target unrewritten.
  return markdown.replace(
    /(!?)\[((?:[^\[\]]|\[[^\]]*\])*)\]\(([^)\s]+)\)/g,
    (whole, bang, text, href) => {
      if (/^(https?:|mailto:|#)/.test(href)) return whole;

      const [rawPath, anchor] = splitAnchor(href);
      const resolved = resolveAgainst(rawPath, linkBase);
      // An image needs the raw bytes; a document link wants the rendered blob.
      const root = bang ? GITHUB_RAW : GITHUB_BLOB;
      return `${bang}[${text}](${root}/${resolved}${anchor})`;
    },
  );
}

/**
 * Resolve a repo-relative link against the containing directory.
 *
 * Throws when `..` would climb past the repo root: the resulting URL would look
 * plausible and 404, which is precisely the silent failure this module exists to
 * avoid, so it must break the build instead.
 */
function resolveAgainst(rawPath: string, linkBase: string): string {
  const segments = linkBase ? linkBase.split("/") : [];
  for (const part of rawPath.split("/")) {
    if (part === "..") {
      if (segments.length === 0) {
        throw new Error(
          `link "${rawPath}" escapes the repository root from base "${linkBase}"`,
        );
      }
      segments.pop();
    } else if (part !== "." && part !== "") {
      segments.push(part);
    }
  }
  return segments.join("/");
}

/** Split "path/to.md#anchor" into its path and its "#anchor" (empty when absent). */
function splitAnchor(href: string): [string, string] {
  const hash = href.indexOf("#");
  return hash === -1 ? [href, ""] : [href.slice(0, hash), href.slice(hash)];
}

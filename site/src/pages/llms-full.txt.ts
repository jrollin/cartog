// `/llms-full.txt` — the machine-readable index of this site.
//
// cartog's users are coding agents, so an agent asked "should I use this, and
// how" should not have to parse a 120 KB built HTML page. This is one small
// plain-text file listing every page and every canonical doc, generated from
// `docs/README.md` so it cannot drift from the docs it advertises.
import type { APIRoute } from "astro";
import { docSourceUrl, docsIndex } from "../lib/docs";

const SITE = "https://www.cartog.dev";

const HEADER = `# cartog: full index

> cartog is a code graph indexer for LLM coding agents. It parses a repository
> into a symbol graph (functions, classes, methods, imports, call edges) that an
> agent queries instead of grepping, and asks the language server to resolve each
> edge, recording which tier resolved it. Single static Rust binary, 18 languages,
> SQLite storage, 100% local: no account, no API keys, nothing leaves the machine.

Every edge carries a provenance tag, so an agent can tell a language-server fact
from a name-match guess: \`lsp\`, \`lsp_external\`, \`lsp_unresolvable\` (asked a real
language server), or one of six ordered heuristic tiers (\`same_file\`,
\`import_path\`, \`same_dir\`, \`parent_scope\`, \`unique_global\`, \`kind_disambig\`).
Surfaced as \`provenance\` in \`--json\` and in MCP tool output.

Search is hybrid: SQLite FTS5 keyword search plus vector KNN, merged by
reciprocal rank fusion and re-ranked by a cross-encoder, returning named symbols
with kind, signature and exact span rather than text chunks.

## Install

\`\`\`
curl -fsSL https://www.cartog.dev/install.sh | sh     # macOS / Linux
cargo install cartog                                  # from source
/plugin marketplace add jrollin/cartog                # Claude Code plugin
\`\`\`

Then, in a repository: \`cartog init && cartog index .\`

## MCP server

\`cartog serve\` speaks the Model Context Protocol over stdio and exposes 16 tools
(18 when the opt-in cross-project tools are enabled with \`[mcp] federated = true\`
or \`cartog serve --federated\`). \`cartog ide\` writes the config into installed
editors. Reference: ${SITE}/usage.html#mcp-server

## Markdown for agents

Two pages are served as plain Markdown:
${SITE}/index.md (the project README) and ${SITE}/usage.md (the usage guide).

Those two are prose overviews. The exhaustive references — every CLI command,
every config key, every MCP tool — live in the repo and are listed under
"Reference" below; \`usage.html\` inlines them all if you would rather read one
page.

## Site pages
`;

/** Site pages, listed before the docs so an agent hits the short answers first. */
const PAGES: Array<[string, string]> = [
  ["index.html", "Landing page: what cartog is, why a graph, edge provenance, alternatives, install"],
  ["usage.html", "Docs page: install, config, semantic search, MCP wiring, every CLI command, troubleshooting (the full reference, inlined)"],
];

export const GET: APIRoute = () => {
  const lines: string[] = [HEADER];

  for (const [page, summary] of PAGES) {
    lines.push(`- [${summary}](${SITE}/${page})`);
  }

  // Canonical docs live in the repo, not on this site, so link them at source.
  let currentGroup = "";
  for (const entry of docsIndex()) {
    if (entry.group !== currentGroup) {
      currentGroup = entry.group;
      lines.push(`\n## ${currentGroup}\n`);
    }
    lines.push(`- [${entry.summary}](${docSourceUrl(entry.docPath)})`);
  }

  lines.push(
    `\n## Source\n`,
    `- Repository: https://github.com/jrollin/cartog`,
    `- Crates: https://crates.io/crates/cartog`,
    `- License: MIT`,
    ``,
  );

  return new Response(lines.join("\n"), {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
};

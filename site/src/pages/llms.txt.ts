// `/llms.txt` — the short index, the convention's entry point.
//
// `/llms-full.txt` lists every page and doc; this is the 20-line version an
// agent reads first to decide whether cartog is relevant at all, and where to
// go next. Kept deliberately small: a short file that is actually read beats a
// complete one that is truncated.
import type { APIRoute } from "astro";

const SITE = "https://www.cartog.dev";

const BODY = `# cartog

> A code graph indexer for LLM coding agents. Parses a repository into a symbol
> graph (functions, classes, methods, imports, call edges) that an agent queries
> instead of grepping, and asks the language server to resolve each edge,
> recording which tier resolved it. Single static Rust binary, 18 languages,
> SQLite storage, 100% local.

- Install: \`curl -fsSL ${SITE}/install.sh | sh\`, then \`cartog init && cartog index .\`
- MCP: \`cartog serve\` exposes 16 tools over stdio (\`cartog ide\` wires your editor).
- Every edge carries a provenance tag, so a language-server fact is
  distinguishable from a name-match guess.

## Docs

- [Full index: every page and every canonical doc](${SITE}/llms-full.txt)
- [Landing page as Markdown](${SITE}/index.md)
- [Usage guide as Markdown](${SITE}/usage.md)
- [Docs page, full reference inlined](${SITE}/usage.html)
- [Source](https://github.com/jrollin/cartog)
`;

export const GET: APIRoute = () =>
  new Response(BODY, { headers: { "Content-Type": "text/plain; charset=utf-8" } });

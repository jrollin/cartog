// `/usage.md` — the docs page as plain Markdown, for agents.
//
// Serves the canonical `docs/usage.md` rather than a Markdown rendering of
// `usage.astro`: the doc is already Markdown, already maintained, and already
// covered by the repo's docs-and-site sync rule. Converting the Astro page
// instead would create a third copy of every fact for a build step to keep
// honest.
//
// The relative links inside the doc point at siblings in `docs/`, which this
// site does not publish, so they are rewritten to their GitHub source.
import type { APIRoute } from "astro";
import { markdownPage } from "../lib/docs";

export const GET: APIRoute = () =>
  markdownPage({
    repoPath: "docs/usage.md",
    canonical: "https://www.cartog.dev/usage.html",
    linkBase: "docs",
  });

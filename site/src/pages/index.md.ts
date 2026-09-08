// `/index.md` — the landing page as plain Markdown, for agents.
//
// Serves the repo `README.md`, which is the canonical prose introduction and is
// already kept in step with the site by the docs-and-site sync rule. Its
// relative links target repo paths, so they are rewritten to GitHub source URLs.
import type { APIRoute } from "astro";
import { markdownPage } from "../lib/docs";

export const GET: APIRoute = () =>
  markdownPage({
    repoPath: "README.md",
    canonical: "https://www.cartog.dev/index.html",
    linkBase: "",
  });

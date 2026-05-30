// Prefix an internal asset/page path with Astro's configured base (/cartog/ in
// production). Keeps dev and prod identical and silences the dev-server
// "must include your base" router warnings. Leave external URLs and bare
// #anchors untouched — only pass repo-relative paths here.
// BASE_URL may or may not carry a trailing slash depending on the resolved
// config, so normalize both sides before joining to avoid "/cartogindex.html".
const BASE = import.meta.env.BASE_URL.replace(/\/$/, "");

export function withBase(path: string): string {
  return `${BASE}/${path.replace(/^\//, "")}`;
}

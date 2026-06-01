# Security Policy

## Supported versions

Only the latest release receives security fixes.

## Secret handling in the index

cartog stores symbol text (and embeddings derived from it) in a local SQLite
database. To reduce the risk of committing hardcoded secrets to that index,
two protections are **on by default**:

- **Best-effort secret redaction.** Common secret patterns (AWS access key IDs,
  GitHub PATs, Slack tokens, Stripe keys, JWTs, and quoted
  `password`/`secret`/`token`/`api_key` assignments) are replaced with
  `[REDACTED_SECRET]` before being stored or embedded. This is **best-effort
  mitigation, not a guarantee** — it favours not mangling real code over total
  recall, so bare high-entropy strings and secrets in unrecognised forms can
  still be indexed. Do not rely on it as your only control against committing
  secrets. Configurable via `[security] redact_secrets` in `.cartog.toml`.
- **Sensitive-file exclusion.** Files such as `.env`, `.env.*`, `*.pem`,
  `*.key`, `*.p12`, `*.pfx`, `id_rsa`, `id_ed25519`, `credentials.json`, and
  `secrets.yml` are never indexed, regardless of the redaction setting.

The index is local by default. If you use `cartog push` to share an index,
review it first: best-effort redaction may not have removed every secret.

## Reporting a vulnerability

**Do not open a public issue for security vulnerabilities.**

Report privately via [GitHub Security Advisories](https://github.com/jrollin/cartog/security/advisories/new).

Include:
- A description of the vulnerability
- Steps to reproduce
- Potential impact

You'll receive a response within 7 days. If confirmed, a fix will be released as soon as possible.

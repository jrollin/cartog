# cartog — Usage

> For editor-specific setup (Neovim keymaps, VS Code tasks, Emacs compile-mode, Zed MCP), see [editor-integration.md](editor-integration.md). For common errors, see [troubleshooting.md](troubleshooting.md).

## Setup

Three install paths, pick whichever matches your environment:

```bash
# 1. Install script (macOS / Linux, no Rust required) — detects platform,
#    downloads + verifies the matching release binary, installs to
#    /usr/local/bin or ~/.local/bin.
curl -fsSL https://www.cartog.dev/install.sh | sh

# 2. Cargo (Rust 1.80+; lets advanced users strip features, e.g. --no-default-features).
cargo install cartog

# 3. Build from source.
cargo build --release && cargo install --path .
```

Override the install location with `CARTOG_INSTALL_DIR=...`; pin a version
with `CARTOG_VERSION=<version>` (e.g. the tag from [Releases](https://github.com/jrollin/cartog/releases)). To upgrade an existing install in place,
`cartog self update`. See [updates.md](updates.md) for the full `cartog self`
command surface, env vars, and rollback.

## Bootstrap

From the repo root, two commands to start. The third is optional.

```bash
cargo install cartog          # one-time, global
cartog init                   # 1. scaffold .cartog.toml (config only)
cartog index                  # 2. build the code graph

cartog ide                    # optional — wire MCP into installed editors
```

| Verb | When you need it | Files |
|---|---|---|
| `cartog init` | Always (once per project) | `.cartog.toml` only |
| `cartog index` | Always (after every code change) | `.cartog/db.sqlite` |
| `cartog ide` | Only if you want MCP in your editor | `.mcp.json`, `.cursor/mcp.json`, `.vscode/mcp.json`, user-scope configs |

Edit `.cartog.toml` between steps 1 and 2 to change the DB path or embedding
provider before any heavy work runs. CLI-only users stop after step 2.

All three commands are idempotent. `cartog init` never overwrites an existing
`.cartog.toml`; `cartog ide` merges entries instead of clobbering (other MCP
servers in the file are preserved); `cartog index` is incremental.

`cartog init` (step 1) is what opts the project in: cartog will not create a
`.cartog/` index for a config-less, never-indexed repo on its own, so
`cartog index` refuses until a `.cartog.toml` exists (or `CARTOG_AUTO_INIT=1`
indexes with defaults, writing no config). An existing index keeps working
without a config. See
[reference/config.md § Index-creation consent gate](reference/config.md#index-creation-consent-gate).

## Configuration

> Full configuration reference (all `.cartog.toml` keys, env vars): **[reference/config.md](reference/config.md)**

To switch embedding providers, see **[how-to/switch-embedding-provider.md](how-to/switch-embedding-provider.md)**.
LSP server overrides and concurrent LSP setup: **[how-to/configure-lsp-servers.md](how-to/configure-lsp-servers.md)**.
S3 push/pull setup: **[how-to/set-up-s3-sync.md](how-to/set-up-s3-sync.md)**.

## Semantic Search

### `cartog rag setup`

Download embedding and re-ranker models from HuggingFace. Run once before using RAG search.

```bash
cartog rag setup
```

**First-time download**: ~230MB of ONNX models (embedding ~80MB + reranker `jinaai/jina-reranker-v1-turbo-en` ~150MB). Models are cached in `~/.cache/cartog/models/` and reused across all projects — subsequent runs are instant. `rag setup` downloads the configured reranker, so pinning `[reranker] model` fetches that one instead.

Note: `rag setup` downloads models for the **local** provider only. When using Ollama, models are managed by the Ollama server — run `ollama pull nomic-embed-text` instead.

### `cartog rag index [path] [--force]`

Build the embedding index for semantic search. Requires `cartog index` and `cartog rag setup` first. Indexes both code symbols and Markdown documents (`.md` files).

```bash
cartog rag index              # embed all symbols + documents in CWD
cartog rag index src/         # embed a subdirectory
cartog rag index --force      # re-embed everything
```

After a cartog upgrade that changes the embedding strategy, `cartog rag index` automatically detects the format change and re-embeds all symbols — no `--force` needed.

### `cartog rag search <query> [--kind <kind>] [--limit N]`

Semantic search over code and documentation — use natural language to find code by what it does, or search project docs alongside code.

```bash
cartog rag search "validate authentication tokens"
cartog rag search "error handling" --kind function
cartog rag search "database connection" --limit 5
cartog rag search "deployment architecture" --kind document
```

Combines keyword (BM25/FTS5) and vector similarity search, merged via RRF, then re-ranked by a cross-encoder model. By default, returns code only; use `--kind document` for docs or `--kind all` for both.

Available `--kind` values: `function`, `class`, `method`, `variable`, `import`, `interface`, `enum`, `enum-member`, `type-alias`, `trait`, `module`, `document`, `macro`, `component`, `all`.

## Claude Plugin

cartog is available as a [Claude Code plugin](https://docs.anthropic.com/en/docs/claude-code) — the recommended way to install for Claude Code users. Plugins bundle skills, scripts, and MCP server configuration into a single installable package.

### Installation

Run these two commands **one at a time** in Claude Code:

1. Register the marketplace:

```bash
/plugin marketplace add jrollin/cartog
```

2. Install the plugin:

```bash
/plugin install cartog@cartog-plugins
```

This installs the plugin from GitHub, which includes:

- The agent skill (behavioral instructions in `skills/cartog/SKILL.md`)
- Setup and install scripts
- Plugin manifest (`.claude-plugin/plugin.json`)

### Plugin Structure

```text
.claude-plugin/
└── plugin.json          # Plugin manifest
skills/
└── cartog/
    ├── SKILL.md         # Agent skill instructions
    ├── scripts/         # install.sh, ensure_indexed.sh
    ├── tests/           # Behavioral evals
    └── references/      # Query cookbook, language support
```

## Agent Skill

cartog also ships as an [Agent Skill](https://agentskills.io) — behavioral instructions that teach your AI agent *when and how* to use cartog, including search routing, refactoring workflows, and fallback heuristics. Use this method for non-Claude Code environments or any LLM with bash access.

### Installation

```bash
npx skills add jrollin/cartog
```

Or install manually:

```bash
cp -r skills/cartog ~/.claude/skills/
```

The plugin wires two Claude Code hooks plus a user-typed skill:

- **SessionStart** (`ensure_indexed.sh`): non-blocking. If the binary is missing, forks a background pipeline (install.sh pinned to the plugin version → optionally `cartog index .` → `cartog rag setup` → `cartog rag index .`) and exits fast — MCP becomes available on the next session. If the binary is present, runs `cartog index .` foreground (incremental, typically <1s) and backgrounds the rest. The background pipeline also runs `cartog self update --apply-pending` (as its first step) so a deferred update armed in a prior session converges here — a catch-up for any SessionEnd apply that was cancelled by session teardown; it runs in the background, so it never blocks the session, and is a no-op when nothing is armed. Prints a one-line drift warning if the installed binary doesn't match the plugin pin — or, when a deferred update is already armed for that pin, "cartog X will be applied when this session ends". Surfaces a "cartog updated to X" breadcrumb once after a completed boundary swap. Missing `.cartog.toml` on an interactive session prints a hint pointing at `cartog init`; on a non-interactive session it exits silently.
- **SessionEnd** (`update_on_exit.sh`): brings the binary in line with the plugin pin at the safe boundary, gated by what the binary can actually do (probed via `self update --help`, not assumed from the version). Deferred-capable binaries (>= 0.20.0) run `cartog self update --apply-pending` once the serve process has exited (no-op if nothing was armed; skipped while a RAG pipeline lock is active; exit 6 means a peer is still live and it retries next session). The peer-wait budget is sized by which lock is blocking, so a hook can't be killed waiting on a lock that will never clear (`Hook cancelled`, [#154](https://github.com/jrollin/cartog/issues/154)): a lock **this project** owns gets a bounded ~2s grace to absorb its own `serve` shutting down, while a lock held **outside** this project is not waited on at all, since it stays held for as long as that session stays open. Nothing is lost when the wait gives up: the intent stays armed and the SessionStart `--at-startup` catch-up applies it, so you never have to stop another session's cartog server (doing so would break that session's live MCP connection). Note the lock name is a hash of the database path, so a second window on the **same** repo is indistinguishable from this project's own peer and takes the bounded grace rather than the no-wait path. Any older binary — the 0.14.0–0.20.0 band (has `cartog self update` but lacks the deferred flags) and pre-0.14.0 (lacks it entirely) — upgrades via the bundled `install.sh` pinned to the plugin version; using a plain `cartog self update` for the 0.14–0.20 band would fetch the latest release and overshoot the pin. The pre-0.14.0 leg is transitional and scheduled for removal.
- **`/cartog-install` skill** (`skills/cartog-install/SKILL.md`): user-typed verb. Installs the binary (when missing) or upgrades it to match the plugin's pinned version. On >=0.14.0 it runs `cartog self update`; inside a Claude Code session, where the MCP server is the live peer (exit 6), it arms a deferred update with `cartog self update --defer` instead. On <0.14.0 it upgrades via `install.sh`. Use this to repair a failed background install or to bring a drifted binary back in sync.

To run the SessionStart steps manually:

```bash
bash skills/cartog/scripts/ensure_indexed.sh
```

### Skill Contents

| File | Purpose |
|------|---------|
| [`SKILL.md`](../skills/cartog/SKILL.md) | Behavioral instructions, commands, and workflows |
| [`skills/cartog/scripts/install.sh`](../skills/cartog/scripts/install.sh) | Automated installation (pre-built binary or cargo install), accepts optional version arg |
| [`scripts/ensure_indexed.sh`](../skills/cartog/scripts/ensure_indexed.sh) | SessionStart hook: install-if-missing + foreground index + background apply-pending/rag setup/index + drift warning |
| [`scripts/update_on_exit.sh`](../skills/cartog/scripts/update_on_exit.sh) | SessionEnd hook: applies a deferred update (`cartog self update --apply-pending`) once the serve peer exits; legacy `install.sh` upgrade for <0.14.0 |
| [`tests/golden_examples.yaml`](../skills/cartog/tests/golden_examples.yaml) | Behavioral test scenarios (expected tool calls per query) |
| [`tests/test_ensure_indexed.sh`](../skills/cartog/tests/test_ensure_indexed.sh) | Bash unit tests for ensure_indexed.sh |
| [`tests/test_update_on_exit.sh`](../skills/cartog/tests/test_update_on_exit.sh) | Bash unit tests for update_on_exit.sh |
| [`tests/test_install.sh`](../skills/cartog/tests/test_install.sh) | Bash unit tests for install.sh |
| [`tests/eval.sh`](../skills/cartog/tests/eval.sh) | LLM-as-judge evaluation via `claude` CLI |
| [`references/query_cookbook.md`](../skills/cartog/references/query_cookbook.md) | Recipes for common navigation patterns |
| [`references/supported_languages.md`](../skills/cartog/references/supported_languages.md) | Language support matrix |

## MCP Server

`cartog serve` runs cartog as an MCP server over stdio, exposing 16 tools by default (18 with the opt-in cross-project tools) for MCP-compatible clients (Claude Code, Cursor, Windsurf, etc.). Each tool carries a human-readable `title` and a `readOnlyHint` annotation: 13 query tools are read-only (including `cartog_trace` for call paths and `cartog_context` for one-shot task bundles); `cartog_index` and `cartog_rag_index` write the index; and `cartog_update` arms a deferred self-update (`readOnlyHint = false` because it writes the machine-level state file, but it never touches the index). Clients can skip approval prompts for the read-only ones.

When `cartog serve --watch` is running and a file changes (or RAG embeddings are still catching up — including symbols whose body was just edited and not yet re-embedded), affected read-tool responses are prefixed with a `⚠️` staleness banner so the agent knows the answer may be momentarily behind the working tree. Read-only secondaries and `cartog serve` without `--watch` never show the banner.

Read tools also declare an `outputSchema` and return `structuredContent` (the typed result mirrored alongside the human-readable text block) so schema-aware clients get validated, machine-readable output. Per the MCP spec, a tool that declares an `outputSchema` must always return `structuredContent`, so the structured copy is kept on every successful call — including large ones. The size cap (`CARTOG_MCP_MAX_BYTES`, default 64 KB) bounds the whole response: an oversized result is trimmed at the *element* level (trailing items dropped) before both the text block and the structured copy are built from the same trimmed set, so both stay under the cap and mutually consistent. The response then carries a notice reporting how many items were omitted and pointing at a narrower tool.

For editor-specific recipes (Neovim keymaps, VS Code tasks, Emacs `compile`, Telescope picker, `cartog watch --json` floating buffer), see **[Editor integration](editor-integration.md)**.

```bash
cartog serve                  # basic MCP server
cartog serve --watch          # auto-re-index + auto-embed when the repo already has embeddings
cartog serve --watch --rag    # force auto-embed even on a not-yet-embedded repo
```

Under `--watch`, embeddings auto-refresh on edits when the repo already has
embeddings (i.e. you have run `cartog rag index` at least once) — no `--rag`
needed. Repos that never used RAG pay nothing: no model loads. Force it on/off
with three controls, highest precedence first: `CARTOG_WATCH_RAG` (env) overrides
`[embedding] auto_embed` (config), which overrides `--rag` (flag). `--rag` without
`--watch` is a no-op (the watcher owns embedding) and warns.

### Per-editor wiring: `cartog ide`

The verb that actually writes MCP configs. Run it once per machine, plus any
time you install a new editor.

```bash
cartog ide                          # configure all installed clients
cartog ide --scope project          # only .mcp.json + .cursor/mcp.json
cartog ide --scope user             # only user-scope clients
cartog ide --client cursor          # one client
cartog ide --dry-run                # preview without writing
```

Supported clients (matches the per-client list below): `claude-code` (project
+ user), `claude-desktop`, `codex`, `cursor`, `gemini`, `opencode`, `vscode`
(project + user), `windsurf`, `zed`, `antigravity`, `kiro` (project + user),
`hermes`. User-scope clients whose config directory does not exist are skipped
(treated as "not installed").

Codex stores all MCP servers in a single user-global `~/.codex/config.toml`,
so cartog writes one per-project section named `cartog-<slug>-<hash8>` (slug
is your project directory name, hash8 is the first 4 bytes of SHA-256 of the
absolute path) to keep multiple projects coexisting. The TOML editor preserves
comments and ordering in the rest of the file.

#### Flag reference

| Flag | Effect |
|---|---|
| `--client <name>` | Target a single client (one of the names above). Default: all matching the scope. |
| `--scope project\|user\|all` | Limit to project-scoped files, user-scoped files, or both (default `all`). |
| `-y`, `--yes` | Skip interactive prompts. Also implied by `--dry-run`, `--client`, `--json`, or a non-TTY stdin. |
| `--dry-run` | Print the planned changes (before/after diff per file) without writing. Implies non-interactive. |
| `--no-watch` | Drop `--watch` from every client's serve args (default wires `serve --watch` for all). |
| `--json` (global) | Emit a structured `IdeReport` on stdout instead of human text. |

#### Troubleshooting

| Symptom | Meaning | Action |
|---|---|---|
| `not modified (config file is not valid JSON); …` | The existing file is JSONC (JSON with comments) or malformed. Cartog refuses to clobber it. | Open the file, fix the JSON manually using the snippets below. |
| `top-level <key> is a <kind> (expected object); refusing to overwrite` | The user file has the right path but the wrong shape at the container key. | Delete the offending key (or the whole file) and re-run. |
| `config directory not found (client likely not installed)` | The user-scope config parent dir doesn't exist on this machine. | Install the editor (or skip with `--client X` targeting only what you have). |
| `0 clients configured` | None of the targeted clients matched. | Check `--client` / `--scope` are compatible (e.g. `cursor` is project-only). |

For deeper diagnostics on the index, embeddings, and language servers, run `cartog doctor`.

### Manual setup (per client)

See **[mcp-setup.md](mcp-setup.md)** for the config-file recipes for every
supported client (Claude Code, Claude Desktop, Cursor, Windsurf, OpenCode,
Zed, Codex CLI, Gemini CLI, VS Code/Copilot, and the generic stdio pattern).

`cartog ide` writes those files for you; the manual page is for users who
want to edit by hand or audit what cartog wrote.

For the full tool list, progress notifications, cancellation, logging, and a Plugin vs MCP vs Skill comparison, see **[reference/mcp-tools.md](reference/mcp-tools.md)**.

## Agents

cartog ships autonomous agents that execute multi-step workflows end-to-end. Agents are bundled in the plugin and available after plugin installation.

### Available Agents

| Agent | Description | Invocation |
|-------|-------------|------------|
| `codebase-onboarding` | Structured onboarding report — adapts to project type and size | `@codebase-onboarding` or "help me understand this project" |
| `refactoring-scout` | Pre-flight blast radius analysis before a refactoring | `@refactoring-scout` or "is it safe to change X?" |

### How Agents Work

Agents differ from the cartog skill:

- **Skill** (reactive): Claude uses cartog commands in response to your questions — you drive the workflow
- **Agent** (autonomous): you give a goal, the agent executes a multi-step plan using cartog, and produces a deliverable

Agents use the CLI via Bash (not MCP), so they work as subagents with isolated context. They are self-contained — no skill injection overhead.

### Manual Installation

If not using the plugin, copy agent definitions to your Claude Code agents directory:

```bash
cp agents/*.md ~/.claude/agents/
```

### Agent: `codebase-onboarding`

Produces a structured onboarding report for an unfamiliar codebase. The agent adapts to the project — a CLI tool gets different treatment than a web API or a library.

**Workflow:**
1. **Discover** — `cartog stats` + `cartog map` + manifest/README to determine project type and scale
2. **Architecture** — trace top-centrality symbols with `callees`/`refs` to map module layout
3. **Entry points** — targeted searches based on project type (CLI commands, API routes, public surface, etc.)
4. **Conventions** — test patterns, code style config, recent git activity

Output: a structured markdown report. Sections that don't apply are omitted.

**Usage:**
```text
@codebase-onboarding
# or
"Use the codebase-onboarding agent to analyze this project"
# or start a session as the agent
claude --agent codebase-onboarding
```

### Agent: `refactoring-scout`

Pre-flight analysis before changing a symbol, module, or file. Maps the full blast radius and produces a go/no-go recommendation.

**Workflow:**
1. **Locate** — confirm exact symbol with `cartog search`, disambiguate if needed
2. **Map blast radius** — `refs` + `impact --depth 3` + `callees` + `hierarchy` (for classes)
3. **Assess risk** — Low / Medium / High based on affected file count and transitive depth
4. **Report** — affected files, risk warnings, and a concrete update checklist

**Usage:**
```text
@refactoring-scout "rename TrackerRepository"
# or
"Is it safe to delete the UtilsHelper class?"
# or start a session as the agent
claude --agent refactoring-scout
```

## Multiple Projects

cartog indexes one project per database. When a question spans two repositories, you do not
need a merged index — you need the other project's database path:

```bash
cartog projects list                 # every indexed project on this machine
cartog search Foo --db /path/to/other/.cartog/db.sqlite
```

Every index, `rag index`, `pull`, watcher re-index, and `serve` startup registers its project,
so there is nothing to enable. Each entry carries a one-line description — from `.cartog.toml`'s
`[project] description`, or the target repo's `README.md` — so an agent can route by intent, not
just by name. Agents get the same rows from the `cartog_list_projects` MCP tool once a project opts in with `[mcp] federated = true` (or `cartog serve --federated`); the cross-project tools are hidden by default. Full
walkthrough: **[how-to/query-another-project.md](how-to/query-another-project.md)**.

## Commands and Configuration

- All CLI commands and flags: **[reference/cli.md](reference/cli.md)**
- All `.cartog.toml` keys and env vars: **[reference/config.md](reference/config.md)**
- MCP tools, progress, cancellation, logging: **[reference/mcp-tools.md](reference/mcp-tools.md)**
- Self-update exit codes and state file: **[reference/exit-codes.md](reference/exit-codes.md)**
- Why the registry stores metadata rather than merging graphs: **[explanation/project-registry.md](explanation/project-registry.md)**


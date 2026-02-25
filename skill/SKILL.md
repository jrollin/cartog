# cartog — Code Graph Navigation Skill

## When to Use

Use cartog **before** reaching for grep, cat, or file reads when you need to:
- Understand the structure of a file → `cartog outline <file>`
- Find who calls a function → `cartog callers <name>`
- See what a function calls → `cartog callees <name>`
- Assess refactoring impact → `cartog impact <name> --depth 3`
- Find all references → `cartog refs <name>`
- Understand class hierarchies → `cartog hierarchy <class>`
- See file dependencies → `cartog deps <file>`

## Workflow Rules

1. **Before you grep or read a file to understand structure**, query cartog first.
2. **Use `cartog outline <file>`** instead of `cat <file>` when you need structure, not content.
3. **Before refactoring**, run `cartog impact <symbol>` to see the blast radius.
4. **Only fall back to grep/read** when cartog doesn't have what you need (e.g., reading actual implementation logic, string literals, config values).
5. **After making code changes**, run `cartog index .` to update the graph.

## Setup

Ensure cartog is indexed before first use:

```bash
bash skill/scripts/ensure_indexed.sh
```

## Commands Reference

### Index (build/rebuild)
```bash
cartog index .                    # Index current directory
cartog index src/                 # Index specific directory
```

### Outline (file structure)
```bash
cartog outline src/auth/tokens.py
```
Output shows symbols with types, signatures, and line ranges — no need to read the file.

### Callers (who calls this?)
```bash
cartog callers validate_token
```

### Callees (what does this call?)
```bash
cartog callees UserService.authenticate
```

### Impact (transitive blast radius)
```bash
cartog impact SessionManager --depth 3
```
Shows everything that transitively depends on a symbol up to N hops.

### Refs (all references)
```bash
cartog refs parse_config
```
Returns calls, imports, inherits — every edge pointing to this name.

### Hierarchy (inheritance tree)
```bash
cartog hierarchy BaseService
```

### Deps (file imports)
```bash
cartog deps src/routes/auth.py
```

### Stats (index summary)
```bash
cartog stats
```

## JSON Output

All commands support `--json` for structured output:
```bash
cartog --json callers validate_token
cartog --json outline src/auth/tokens.py
```

## Decision Heuristics

| I need to... | Use |
|-------------|-----|
| Know what's in a file | `cartog outline <file>` |
| Find usages of a function | `cartog callers <name>` |
| Understand what a function does at a high level | `cartog callees <name>` |
| Check if a change is safe | `cartog impact <name>` |
| Find all references (imports, calls, inherits) | `cartog refs <name>` |
| Understand class hierarchy | `cartog hierarchy <class>` |
| See file dependencies | `cartog deps <file>` |
| Read actual implementation logic | `cat <file>` (cartog can't help here) |
| Search for string literals / config | `grep` (cartog indexes structure, not content) |

## Limitations

- Structural/heuristic resolution, not full semantic. ~90% accuracy for cross-file references.
- Currently supports: Python, TypeScript/JavaScript, Rust, Go. Java planned.
- Does not index string literals, comments (except docstrings), or config values.
- Method resolution is name-based — `foo.bar()` resolves `bar`, not `Foo.bar` specifically.

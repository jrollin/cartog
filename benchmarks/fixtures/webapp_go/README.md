# webapp_go

Synthetic Go web application fixture for cartog benchmarks. Models auth, tokens, routes, middleware, database, cache, events, and validators.

- **45 files, ~3,260 LOC**
- Module: `webapp_go` (see `go.mod`)
- Domain: same as all 5 fixtures (cross-language comparison)

## Validate

```bash
go build ./...
```

Note: Go uses `MixedCaps` (not `SCREAMING_SNAKE_CASE`) for constants. Composition via struct embedding replaces class inheritance.

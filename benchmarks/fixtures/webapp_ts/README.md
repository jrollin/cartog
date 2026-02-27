# webapp_ts

Synthetic TypeScript web application fixture for cartog benchmarks. Models auth, tokens, routes, middleware, database, cache, events, and validators.

- **48 files, ~2,530 LOC**
- Domain: same as all 5 fixtures (cross-language comparison)

## Validate

```bash
npx tsc --noEmit --strict --esModuleInterop --skipLibCheck
```

Note: `@types/node` is not included; use `--skipLibCheck` to avoid external type errors.

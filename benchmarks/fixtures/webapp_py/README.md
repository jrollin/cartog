# webapp_py

Synthetic Python web application fixture for cartog benchmarks. Models auth, tokens, routes, middleware, database, cache, events, and validators.

- **69 files, ~4,050 LOC**
- Domain: same as all 5 fixtures (cross-language comparison)

## Validate

```bash
python3 -m py_compile <file>             # single file
find . -name '*.py' -exec python3 -m py_compile {} +  # all files
```

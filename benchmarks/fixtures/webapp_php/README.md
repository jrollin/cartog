# webapp_php

Synthetic PHP web application fixture for cartog benchmarks. Models auth, tokens,
routes, middleware, database, events, payments, and validators.

- **18 files**, PSR-4 namespaced (`App\…`), PHP 8.1+ (typed properties,
  constructor promotion, `enum`)
- Domain: same as all 6 fixtures (cross-language comparison)

## Validate

```bash
php -l <file>                                    # single file
find . -name '*.php' -exec php -l {} \;          # all files

# without a local PHP install:
docker run --rm -v "$PWD:/app" -w /app php:8.3-cli \
  sh -c 'for f in $(find . -name "*.php"); do php -l "$f"; done'
```

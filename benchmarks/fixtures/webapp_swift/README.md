# webapp_swift

Synthetic Swift fixture for cartog benchmarks. A small SwiftPM executable that
mirrors the other `webapp_*` fixtures' domain (auth service hierarchy, models,
database queries, routes/handlers, middleware, validators, a logger), so the 13
benchmark scenarios map across languages.

Covers each Swift paradigm cartog extracts: classes, structs, `final class`,
protocols + conformance, enums with raw values, extensions, typealiases,
`init`/`deinit`, computed properties, `async`/`throws`, access modifiers
(`private`/`public`/default-internal), and `///` doc comments.

Scenario anchors: `validateToken` (callers), `AuthService` (impact),
`BaseService` (hierarchy), `handleLogin` (deep call chain),
`routes/AuthRoutes.swift` (deps), `TokenError` (type refs), `Token`/`validate`
(search/ambiguity), `getLogger` (high fanout), `DatabaseConnection` (deep impact).

## Validate

```bash
swift build
```

The Makefile `check-swift` target runs this natively when the Swift toolchain is
on PATH, else inside the `swift:latest` Docker image.

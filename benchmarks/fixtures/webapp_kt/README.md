# webapp_kt

Synthetic Kotlin fixture for cartog benchmarks. A small Gradle/Kotlin JVM app that
mirrors the other `webapp_*` fixtures' domain (auth service hierarchy, models,
database queries, routes/handlers, middleware, validators, a logger), so the 13
benchmark scenarios map across languages.

Covers each Kotlin paradigm cartog extracts: classes, `data class`, `sealed class`,
`enum class`, interfaces + implementation, `object`/companion objects, primary and
secondary constructors, `suspend` functions, access modifiers
(`private`/`internal`/default-public), and KDoc (`/** */`) comments.

Scenario anchors: `validateToken` (callers), `AuthService` (impact),
`BaseService` (hierarchy), `handleLogin` (deep call chain),
`routes/AuthRoutes.kt` (deps), `TokenError` (type refs), `Token`/`validate`
(search/ambiguity), `getLogger` (high fanout), `DatabaseConnection` (deep impact).

## Validate

```bash
gradle build
```

The Makefile `check-kt` target runs this natively when the Kotlin/Gradle toolchain
is on PATH, else inside a pinned `gradle` Docker image.

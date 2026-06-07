# Pinned Kotlin *compiler* image for reproducible `make check-kt` fixture
# validation. Distinct from kotlin.Dockerfile (which ships the kotlin-language-
# SERVER for edge-resolution benchmarks); this ships kotlinc (the compiler).
#
# There is no maintained official Kotlin image (the old zenika/kotlin tag is
# gone and never published 1.9), so we bake the pinned standalone compiler into
# a layer here — cached after `docker build`, so check-kt needs no network at
# run time (parity with the other pinned check-* images).
#
# Build (from repo root):
#   docker build -t cartog-kotlinc:stable -f benchmarks/lsp-images/kotlinc.Dockerfile benchmarks/lsp-images
# Bump the compiler version with --build-arg KOTLIN_VERSION=<v> (keep it in
# sync with the Makefile's KOTLIN_VERSION default).
#
# Must stay a *-jdk* base: the unzip step uses `jar` (the base has no `unzip`),
# and kotlinc shells out to the JDK at runtime.
FROM eclipse-temurin:17-jdk
ARG KOTLIN_VERSION=1.9.24
RUN curl -fsSL -o /tmp/kotlin.zip \
        "https://github.com/JetBrains/kotlin/releases/download/v${KOTLIN_VERSION}/kotlin-compiler-${KOTLIN_VERSION}.zip" \
    && (cd /opt && jar xf /tmp/kotlin.zip) \
    && rm /tmp/kotlin.zip \
    && ln -s /opt/kotlinc/bin/kotlinc /usr/local/bin/kotlinc
# No ENTRYPOINT: the check-kt recipe invokes `kotlinc` explicitly.

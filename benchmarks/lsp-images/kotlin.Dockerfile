# Dockerized kotlin-language-server for reproducible edge-resolution benchmarks.
#
# Build:  docker build -t cartog-lsp-kotlin:stable -f kotlin.Dockerfile .
# Use via .cartog.toml (ServerSpec args are empty; the bare launcher reads stdio):
#   [lsp.kotlin]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-kotlin:stable"]
#
# Notes: the no-args ENTRYPOINT is load-bearing — the upstream image's default
# `--tcpServerPort` would break the stdio handshake. Needs a full JDK at runtime
# (the launcher shells out to the bundled Kotlin compiler). First requests warm
# up the compiler frontend (several seconds on a large repo).
FROM eclipse-temurin:17-jdk
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl unzip ca-certificates \
    && rm -rf /var/lib/apt/lists/*
ARG KLS_VERSION=1.3.13
RUN curl -fsSL -o /tmp/kls.zip \
        "https://github.com/fwcd/kotlin-language-server/releases/download/${KLS_VERSION}/server.zip" \
    && unzip -q /tmp/kls.zip -d /opt \
    && rm /tmp/kls.zip \
    && ln -s /opt/server/bin/kotlin-language-server /usr/local/bin/kotlin-language-server
ENTRYPOINT ["kotlin-language-server"]

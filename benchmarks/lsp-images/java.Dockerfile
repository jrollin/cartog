# Dockerized Eclipse JDT language server (jdtls) for edge-resolution benchmarks.
#
# Build:  docker build -t cartog-lsp-java:stable -f java.Dockerfile .
# Use via .cartog.toml (ServerSpec args are empty; the bare wrapper reads stdio):
#   [lsp.java]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-java:stable"]
#
# Notes: jdtls is a Python wrapper (python3 is mandatory). Its workspace-data dir
# defaults to $HOME/.cache/jdtls/<hash> keyed on the cwd basename — HOME=/tmp +
# a world-writable cache keeps it usable under any container UID. Startup is
# heavy (Eclipse/OSGi + JVM); the benchmark's LSP readiness timeout must tolerate
# it. Without pom.xml/build.gradle, jdtls falls back to source-only resolution.
# The milestone tarball carries a build timestamp, so it's resolved from the
# milestone's latest.txt at build time rather than hardcoded.
FROM eclipse-temurin:21-jdk
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*
ARG JDTLS_VERSION=1.43.0
RUN base="https://download.eclipse.org/jdtls/milestones/${JDTLS_VERSION}" \
    && tarball="$(wget -qO- "${base}/latest.txt")" \
    && mkdir -p /opt/jdtls \
    && wget -qO /tmp/jdtls.tar.gz "${base}/${tarball}" \
    && tar -xzf /tmp/jdtls.tar.gz -C /opt/jdtls \
    && rm /tmp/jdtls.tar.gz \
    && ln -s /opt/jdtls/bin/jdtls /usr/local/bin/jdtls
ENV HOME=/tmp
RUN mkdir -p /tmp/.cache/jdtls && chmod -R 0777 /tmp/.cache
ENTRYPOINT ["jdtls"]

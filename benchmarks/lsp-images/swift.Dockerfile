# Dockerized sourcekit-lsp for reproducible edge-resolution benchmarks.
#
# Build (from repo root):  docker build -t cartog-lsp-swift:stable -f benchmarks/lsp-images/swift.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml (ServerSpec args are empty; the bare server reads stdio):
#   [lsp.swift]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-swift:stable"]
#
# Notes: sourcekit-lsp ships in the full Swift toolchain (~2.5-3 GB; do NOT use
# swift:6.1-slim — it omits libsourcekitdInProc.so and won't start). Cross-module
# resolution is best after a `swift build` produces an index store; bare same-file
# resolution works without one.
FROM swift:6.1
ENTRYPOINT ["sourcekit-lsp"]

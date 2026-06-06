# Dockerized Dart LSP server for reproducible edge-resolution benchmarks.
#
# `dart language-server` ships with the SDK, so there is nothing to install.
# Build (from repo root):  docker build -t cartog-lsp-dart:stable -f benchmarks/lsp-images/dart.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml:
#   [lsp.dart]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-dart:stable"]
#
# The ${ROOT}:${ROOT} bind mount mirrors the host path inside the container so
# the file:// URIs cartog exchanges with the server resolve identically on both
# sides (see docs/usage.md, "LSP server overrides").
FROM dart:stable
ENTRYPOINT ["dart", "language-server", "--protocol=lsp"]

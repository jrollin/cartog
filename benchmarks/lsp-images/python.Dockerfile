# Dockerized pyright for reproducible edge-resolution benchmarks.
#
# Build:  docker build -t cartog-lsp-python:stable -f python.Dockerfile .
# Use via .cartog.toml (the CMD supplies --stdio; do NOT append it):
#   [lsp.python]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-python:stable"]
#
# Thin wrapper over the upstream lspcontainers image (smaller + maintained, and
# verified to give the identical resolution rate as a self-built image — see
# benchmarks/README.md). To self-build instead, swap the FROM for a base that
# carries Node + pyright and add a `pyright-langserver --stdio` ENTRYPOINT.
FROM lspcontainers/pyright-langserver:latest

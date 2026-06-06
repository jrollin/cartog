# Dockerized pyright for reproducible edge-resolution benchmarks.
#
# Build (from repo root):  docker build -t cartog-lsp-python:stable -f benchmarks/lsp-images/python.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml (the CMD supplies --stdio; do NOT append it):
#   [lsp.python]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-python:stable"]
#
# Thin wrapper over the upstream lspcontainers image (smaller + maintained, and
# verified to give the identical resolution rate as host — see benchmarks/README.md).
# To self-build instead, swap the FROM for a base that carries Node + pyright and
# add a `pyright-langserver --stdio` ENTRYPOINT.
#
# Pinned by digest for reproducibility (`:latest` drifts). Re-pin with:
#   docker pull lspcontainers/pyright-langserver:latest
#   docker inspect --format '{{index .RepoDigests 0}}' lspcontainers/pyright-langserver:latest
FROM lspcontainers/pyright-langserver@sha256:1fed00fceedecae7d7de8b1c0d11d1f07eb32cc8c477bbfddfb57df677039838

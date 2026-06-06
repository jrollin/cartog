# Dockerized typescript-language-server for reproducible edge-resolution benchmarks.
# Serves the typescript / tsx / javascript cartog languages (one server, one image).
#
# Build (from repo root):  docker build -t cartog-lsp-typescript:stable -f benchmarks/lsp-images/typescript.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml (the CMD supplies --stdio; do NOT append it):
#   [lsp.typescript]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-typescript:stable"]
#
# Thin wrapper over the upstream lspcontainers image (smaller + maintained, and
# verified to give the identical resolution rate as host — see benchmarks/README.md).
# To self-build instead, swap the FROM for a node base and add
# `npm i -g typescript-language-server typescript` + a `--stdio` ENTRYPOINT.
#
# Pinned by digest for reproducibility (`:latest` drifts). Re-pin with:
#   docker pull lspcontainers/typescript-language-server:latest
#   docker inspect --format '{{index .RepoDigests 0}}' lspcontainers/typescript-language-server:latest
FROM lspcontainers/typescript-language-server@sha256:a27fbcb8eafcb3c09f68eaea7971ad81fe633ac87636675f24f50c90cc49072a

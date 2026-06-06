# Dockerized typescript-language-server for reproducible edge-resolution benchmarks.
# Serves the typescript / tsx / javascript cartog languages (one server, one image).
#
# Build:  docker build -t cartog-lsp-typescript:stable -f typescript.Dockerfile .
# Use via .cartog.toml (the CMD supplies --stdio; do NOT append it):
#   [lsp.typescript]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-typescript:stable"]
#
# Thin wrapper over the upstream lspcontainers image (smaller + maintained, and
# verified to give the identical resolution rate as a self-built image — see
# benchmarks/README.md). To self-build instead, swap the FROM for a node base and
# add `npm i -g typescript-language-server typescript` + a `--stdio` ENTRYPOINT.
FROM lspcontainers/typescript-language-server:latest

# Dockerized intelephense for reproducible edge-resolution benchmarks.
#
# Build:  docker build -t cartog-lsp-php:stable -f php.Dockerfile .
# Use via .cartog.toml (the ENTRYPOINT supplies --stdio; do NOT append it):
#   [lsp.php]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-php:stable"]
#
# Notes: intelephense is a Node program (no PHP runtime needed — it has its own
# parser). The free tier covers definitions/references/symbols, which is what the
# benchmark measures; no license key is baked in.
FROM node:22-bookworm-slim
RUN npm install -g intelephense@1.18.4 && npm cache clean --force
ENTRYPOINT ["intelephense", "--stdio"]

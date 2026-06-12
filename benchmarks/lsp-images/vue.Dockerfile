# Dockerized Vue language server (Volar) for reproducible edge-resolution benchmarks.
# Serves the `vue` cartog language; SFC <script> edges resolve through it.
#
# Build (from repo root):  docker build -t cartog-lsp-vue:stable -f benchmarks/lsp-images/vue.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml (the ENTRYPOINT supplies --stdio; do NOT append it):
#   [lsp.vue]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-vue:stable"]
#
# Self-built on a Node base (no maintained upstream image). The ENTRYPOINT
# reproduces the ServerSpec: `vue-language-server --stdio`.
#
# Pin the base by digest before relying on the numbers (`:slim` drifts):
#   docker pull node:22-bookworm-slim
#   docker inspect --format '{{index .RepoDigests 0}}' node:22-bookworm-slim
# then replace the tag below with node:22-bookworm-slim@sha256:<digest>.
FROM node:22-bookworm-slim
RUN npm i -g @vue/language-server@2 typescript
ENTRYPOINT ["vue-language-server", "--stdio"]

# Dockerized rust-analyzer for reproducible edge-resolution benchmarks.
#
# Build:  docker build -t cartog-lsp-rust:stable -f rust.Dockerfile .
# Use via .cartog.toml (the ENTRYPOINT supplies the LSP args; do NOT append any):
#   [lsp.rust]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-rust:stable"]
#
# Notes: rust-analyzer needs the full toolchain at runtime (it shells out to
# cargo); rust:slim already carries it. A repo pinning a different toolchain via
# rust-toolchain.toml triggers a one-time rustup download (needs network).
FROM rust:1.83-slim
RUN rustup component add rust-analyzer
ENTRYPOINT ["rust-analyzer"]

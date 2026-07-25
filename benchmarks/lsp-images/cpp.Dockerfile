# Dockerized clangd for reproducible C++ edge-resolution benchmarks.
#
# Build (from repo root):  docker build -t cartog-lsp-cpp:stable -f benchmarks/lsp-images/cpp.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml (ServerSpec args are empty; the bare launcher reads stdio):
#   [lsp.cpp]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-cpp:stable"]
#
# `-i` (never `-t`): clangd speaks LSP over stdio, so stdin must be attached but
# no TTY is allocated. A command-override server is launched with
# `processId: null` (cartog already sends this for overrides) so clangd does not
# fail its parent-liveness check — the host cartog PID is absent from the
# container PID namespace.
#
# Path mirroring (`-v ${ROOT}:${ROOT} -w ${ROOT}`) is mandatory: cartog exchanges
# host-absolute `file://` URIs, and clangd resolves the compile database relative
# to the file's own path.
#
# COMPILE DATABASE: clangd needs `compile_commands.json` or `compile_flags.txt`
# at the project root to know the include paths and standard. Without one it
# guesses bare flags and cross-file includes go unresolved. The `webapp_cpp`
# fixture ships a `compile_flags.txt` for exactly this reason.
#
# The fixture is dependency-free (standard library only), so the container runs
# fully offline once built — the same invariant as go's GOTOOLCHAIN=local and
# csharp's BCL-only fixture.
#
# Pin: Debian trixie ships clangd 19 as `clangd-19`; the unversioned `clangd`
# package is a floating alias, so install the versioned binary and symlink it.
# Do NOT float to a bare `clangd`.
FROM debian:trixie-20260112-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends clangd-19 \
 && rm -rf /var/lib/apt/lists/* \
 && ln -s /usr/bin/clangd-19 /usr/local/bin/clangd
ENTRYPOINT ["clangd"]

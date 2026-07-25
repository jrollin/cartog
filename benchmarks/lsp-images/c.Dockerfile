# Dockerized clangd for reproducible C edge-resolution benchmarks.
#
# Build (from repo root):  docker build -t cartog-lsp-c:stable -f benchmarks/lsp-images/c.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml (ServerSpec args are empty; the bare launcher reads stdio):
#   [lsp.c]
#   command = ["docker", "run", "--rm", "-i", "--user", "1000:1000",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-c:stable"]
#
# `--user <uid>:<gid>` (your own `id -u`/`id -g`) drops root and keeps the
# bind-mounted repo readable. Verified byte-identical resolution to a root run.
# It is passed at launch rather than baked in as `USER`, because a fixed uid in
# the image cannot also match an arbitrary host uid on the bind mount — the same
# reason the Makefile's `check_lang` uses `--user $(id -u):$(id -g)`. clangd
# needs no write access to the tree (it creates no `.cache/clangd` here, as the
# repo ships a `compile_flags.txt` and background indexing stays off).
#
# Same server binary as cpp.Dockerfile (clangd serves both languages); kept as a
# separate image so each `ServerSpec` language has its own `cartog-lsp-<lang>:stable`
# tag, which is what `resolution_rate.sh --docker-lsp` looks up.
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
# guesses bare flags and cross-file includes go unresolved. The `webapp_c`
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

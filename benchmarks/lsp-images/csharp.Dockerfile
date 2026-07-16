# Dockerized csharp-ls for reproducible edge-resolution benchmarks.
#
# Build (from repo root):  docker build -t cartog-lsp-csharp:stable -f benchmarks/lsp-images/csharp.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml (ServerSpec args are empty; the bare launcher reads stdio):
#   [lsp.csharp]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-csharp:stable"]
#
# `-i` (never `-t`): csharp-ls speaks LSP over stdio, so stdin must be attached
# but no TTY is allocated. A command-override server is launched with
# `processId: null` (cartog already sends this for overrides) so csharp-ls does
# not fail its parent-liveness check — the host cartog PID is absent from the
# container PID namespace.
#
# csharp-ls auto-discovers the nearest `.sln`/`.csproj` under `-w ${ROOT}`. The
# `webapp_csharp` fixture is BCL-only (System.* only, zero PackageReference), so
# csharp-ls needs no NuGet restore and the container runs fully offline — the
# same invariant as go's GOTOOLCHAIN=local and kotlin's bundled runtime.
#
# Pins (both verified against upstream at authoring time — re-confirm before an
# image rebuild): csharp-ls 0.20.0 is the last release targeting net9.0
# (0.21.0+ target net10.0), matched to the .NET SDK 9.0.316 patch tag published
# on mcr.microsoft.com/dotnet/sdk. Do NOT float to a bare `9.0`/`10.0` major.
FROM mcr.microsoft.com/dotnet/sdk:9.0.316
RUN dotnet tool install --global --version 0.20.0 csharp-ls
ENV PATH="/root/.dotnet/tools:${PATH}"
ENTRYPOINT ["csharp-ls"]

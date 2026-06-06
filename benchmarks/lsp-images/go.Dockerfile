# Dockerized gopls for reproducible edge-resolution benchmarks.
#
# Build:  docker build -t cartog-lsp-go:stable -f go.Dockerfile .
# Use via .cartog.toml (the ENTRYPOINT supplies `serve`; do NOT append it):
#   [lsp.go]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-go:stable"]
#
# Notes: gopls type-checks the repo, so cross-package edges need the module's
# deps resolvable (network into GOMODCACHE, or a vendored ./vendor). /go stays
# writable for the cache. GOTOOLCHAIN=local fails fast if go.mod needs > 1.23
# rather than silently downloading a toolchain.
FROM golang:1.23
RUN go install golang.org/x/tools/gopls@v0.16.2
ENV GOPATH=/go GOCACHE=/go/cache GOMODCACHE=/go/pkg/mod GOFLAGS=-mod=mod GOTOOLCHAIN=local
ENV PATH=/go/bin:$PATH
ENTRYPOINT ["gopls", "serve"]

# Dockerized ruby-lsp for reproducible edge-resolution benchmarks.
#
# Build (from repo root):  docker build -t cartog-lsp-ruby:stable -f benchmarks/lsp-images/ruby.Dockerfile benchmarks/lsp-images
# Use via .cartog.toml (ServerSpec args are empty; the bare server reads stdio):
#   [lsp.ruby]
#   command = ["docker", "run", "--rm", "-i",
#              "-v", "${ROOT}:${ROOT}", "-w", "${ROOT}", "cartog-lsp-ruby:stable"]
#
# Notes: with a Gemfile in the workspace, ruby-lsp composes a project bundle into
# .ruby-lsp/ on first request (slow; needs a writable workspace, which the bind
# mount is). For first-party edge resolution run the fixtures without a Gemfile
# to skip bundle compose.
#
# ruby-lsp pulls `prism`, whose native extension needs a C toolchain to compile.
# build-essential is installed for the gem build and removed in the same layer so
# only the compiled prism .so stays — the image keeps the -slim footprint.
FROM ruby:3.3-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential \
    && gem install --no-document ruby-lsp \
    && apt-get purge -y build-essential \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*
ENTRYPOINT ["ruby-lsp"]

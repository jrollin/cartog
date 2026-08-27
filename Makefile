.PHONY: check check-rust check-fixtures check-fixtures-docker check-skill check-py check-ts check-go check-rs check-rb check-java check-php check-dart check-swift check-csharp check-c check-cpp check-kt check-vue check-svelte check-astro check-install-script tla loom bench bench-memory bench-resolution bench-resolution-docker bench-resolution-scale lsp-images bench-criterion bench-rag bench-onnx bench-agent eval-skill eval-agents

# --- Full integrity check ---

check: check-rust check-fixtures check-skill check-install-script ## Run all integrity checks

# --- Install script ---
#
# site/public/install.sh is the single canonical curl-target script; GitHub
# Pages serves it at https://www.cartog.dev/install.sh.

check-install-script: ## Syntax-check the install script
	@echo "==> Checking install.sh..."
	@sh -n site/public/install.sh
	@echo "    OK"

# --- Fixture validation helper ---
#
# Validates a fixture codebase with the native toolchain when present, else
# falls back to a pinned official Docker image, else fails hard.
# $(1)=probe tool  $(2)=docker image  $(3)=native command  $(4)=docker command
# The Docker branch mounts benchmarks/fixtures at /fix as the working dir;
# $(4) addresses the language subdir from there and directs any compiler
# output to container-internal /tmp paths so runs leave no root-owned files
# in the working tree. Set FORCE_DOCKER=1 to skip the native probe.
define check_lang
	@if [ -z "$(FORCE_DOCKER)" ] && command -v $(1) > /dev/null 2>&1; then \
		$(3); \
	elif command -v docker > /dev/null 2>&1; then \
		echo "    $(1) not found, using Docker ($(2))"; \
		docker run --rm --user $$(id -u):$$(id -g) \
			-v "$(CURDIR)/benchmarks/fixtures:/fix" -w /fix $(2) sh -c '$(4)'; \
	else \
		echo "    ERROR: neither $(1) nor docker available"; exit 1; \
	fi
	@echo "    OK"
endef

# --- Rust project checks ---

check-rust: ## cargo fmt + clippy + test
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

# --- Fixture syntax/build checks ---

check-fixtures: check-py check-ts check-go check-rs check-rb check-java check-php check-dart check-swift check-csharp check-c check-cpp check-kt ## Validate all fixture codebases

check-fixtures-docker: ## Validate all fixture codebases via Docker (reproducible)
	@$(MAKE) check-fixtures FORCE_DOCKER=1

check-py: ## Validate Python fixtures (py_compile, falls back to Docker)
	@echo "==> Checking Python fixtures..."
	$(call check_lang,python3,python:3.12-slim,\
		find benchmarks/fixtures/webapp_py -name "*.py" -exec python3 -m py_compile {} +,\
		find webapp_py -name "*.py" -exec python3 -m py_compile {} +)

check-ts: ## Validate TypeScript fixtures (tsc -p, falls back to Docker)
	@echo "==> Checking TypeScript fixtures..."
	$(call check_lang,npx,node:22-slim,\
		cd benchmarks/fixtures/webapp_ts && npx --yes --package typescript@5 -- tsc -p tsconfig.json,\
		cd webapp_ts && HOME=/tmp npm_config_cache=/tmp/npm npx --yes --package typescript@5 -- tsc -p tsconfig.json)

check-go: ## Validate Go fixtures (go build, falls back to Docker)
	@echo "==> Checking Go fixtures..."
	$(call check_lang,go,golang:1.23,\
		cd benchmarks/fixtures/webapp_go && go build ./...,\
		cd webapp_go && GOCACHE=/tmp/gocache GOPATH=/tmp/gopath go build ./...)

check-rs: ## Validate Rust fixtures (cargo check, falls back to Docker)
	@echo "==> Checking Rust fixtures..."
	$(call check_lang,cargo,rust:slim,\
		cd benchmarks/fixtures/webapp_rs && cargo check 2>/dev/null,\
		cd webapp_rs && CARGO_TARGET_DIR=/tmp/target CARGO_HOME=/tmp/cargo cargo check 2>/dev/null)

check-rb: ## Validate Ruby fixtures (ruby -c, falls back to Docker)
	@echo "==> Checking Ruby fixtures..."
	$(call check_lang,ruby,ruby:3.3-slim,\
		find benchmarks/fixtures/webapp_rb -name "*.rb" -exec ruby -c {} + > /dev/null,\
		find webapp_rb -name "*.rb" -exec ruby -c {} + > /dev/null)

check-java: ## Validate Java fixtures (javac, falls back to Docker)
	@echo "==> Checking Java fixtures..."
	$(call check_lang,javac,eclipse-temurin:21-jdk,\
		mkdir -p /tmp/cartog_java_check && cd benchmarks/fixtures/webapp_java && javac -sourcepath . $$(find . -name "*.java" | sort) -d /tmp/cartog_java_check,\
		cd webapp_java && javac -sourcepath . $$(find . -name "*.java" | sort) -d /tmp/out)

check-php: ## Validate PHP fixtures (php -l, falls back to Docker)
	@echo "==> Checking PHP fixtures..."
	$(call check_lang,php,php:8.3-cli,\
		find benchmarks/fixtures/webapp_php -name '*.php' -exec php -l {} + > /dev/null,\
		for f in $$(find webapp_php -name "*.php"); do php -l "$$f" > /dev/null || exit 1; done)

check-dart: ## Validate Dart fixtures (dart analyze, falls back to Docker)
	@echo "==> Checking Dart fixtures..."
	$(call check_lang,dart,dart:stable,\
		cd benchmarks/fixtures/webapp_dart && dart analyze --fatal-infos,\
		cd webapp_dart && HOME=/tmp PUB_CACHE=/tmp/pub dart analyze --fatal-infos)

check-swift: ## Validate Swift fixtures (swift build, falls back to Docker)
	@echo "==> Checking Swift fixtures..."
	$(call check_lang,swift,swift:6.1,\
		cd benchmarks/fixtures/webapp_swift && swift build,\
		cd webapp_swift && HOME=/tmp swift build --cache-path /tmp/swiftpm --scratch-path /tmp/swiftbuild)

check-csharp: ## Validate C# fixtures (dotnet build, falls back to Docker)
	@echo "==> Checking C# fixtures..."
	$(call check_lang,dotnet,mcr.microsoft.com/dotnet/sdk:9.0.316,\
		cd benchmarks/fixtures/webapp_csharp && dotnet build --nologo -v q,\
		cd webapp_csharp && DOTNET_CLI_HOME=/tmp DOTNET_CLI_TELEMETRY_OPTOUT=1 NUGET_PACKAGES=/tmp/nuget dotnet build --nologo -v q)

# C / C++ fixtures are library-style (no single entry point to link), so both
# targets syntax-check every translation unit instead of building a binary.
# -fsyntax-only writes no object files, so the Docker branch leaves no
# root-owned droppings in the working tree and needs no cache redirection.
check-c: ## Validate C fixtures (cc -fsyntax-only, falls back to Docker)
	@echo "==> Checking C fixtures..."
	$(call check_lang,cc,silkeh/clang:19,\
		find benchmarks/fixtures/webapp_c -name "*.c" -exec cc -std=c11 -fsyntax-only -I benchmarks/fixtures/webapp_c {} +,\
		find webapp_c -name "*.c" -exec clang -std=c11 -fsyntax-only -I webapp_c {} +)

check-cpp: ## Validate C++ fixtures (c++ -fsyntax-only, falls back to Docker)
	@echo "==> Checking C++ fixtures..."
	$(call check_lang,c++,silkeh/clang:19,\
		find benchmarks/fixtures/webapp_cpp -name "*.cpp" -exec c++ -std=c++17 -fsyntax-only -I benchmarks/fixtures/webapp_cpp {} +,\
		find webapp_cpp -name "*.cpp" -exec clang++ -std=c++17 -fsyntax-only -I webapp_cpp {} +)

# Kotlin has no maintained official compiler image (the old zenika/kotlin tag is
# gone — it never published a 1.9 tag). The Docker fallback uses a project-pinned
# image (benchmarks/lsp-images/kotlinc.Dockerfile) built on demand: the compiler
# download is baked into a cached layer, so check-kt needs no network at run time
# (parity with the other pinned check-* images, and works offline after the first
# build). KOTLIN_VERSION feeds both the image build-arg and the image tag, so a
# version bump rebuilds. Not the generic check_lang macro because that can only
# `docker run` a fixed image, not build-if-missing.
KOTLIN_VERSION ?= 1.9.24
KOTLINC_IMAGE := cartog-kotlinc:$(KOTLIN_VERSION)
check-kt: ## Validate Kotlin fixtures (kotlinc compile, falls back to pinned Docker image)
	@echo "==> Checking Kotlin fixtures..."
	@if [ -z "$(FORCE_DOCKER)" ] && command -v kotlinc > /dev/null 2>&1; then \
		cd benchmarks/fixtures/webapp_kt && kotlinc src -include-runtime -d /tmp/webapp_kt.jar; \
	elif command -v docker > /dev/null 2>&1; then \
		docker image inspect $(KOTLINC_IMAGE) > /dev/null 2>&1 || { \
			echo "    building $(KOTLINC_IMAGE) (one-time; cached after)"; \
			docker build --build-arg KOTLIN_VERSION=$(KOTLIN_VERSION) \
				-t $(KOTLINC_IMAGE) -f benchmarks/lsp-images/kotlinc.Dockerfile benchmarks/lsp-images || exit 1; \
		}; \
		echo "    kotlinc not found, using Docker ($(KOTLINC_IMAGE))"; \
		docker run --rm --user $$(id -u):$$(id -g) \
			-v "$(CURDIR)/benchmarks/fixtures:/fix" -w /fix $(KOTLINC_IMAGE) \
			sh -c 'export HOME=/tmp; cd /fix/webapp_kt && bash /opt/kotlinc/bin/kotlinc src -include-runtime -d /tmp/webapp_kt.jar'; \
	else \
		echo "    ERROR: neither kotlinc nor docker available"; exit 1; \
	fi
	@echo "    OK"

# SFC fixtures (Vue/Svelte/Astro) type-check with the framework's own checker via
# npx. These pull a full framework toolchain on first run, so they're NOT wired
# into the `check-fixtures` gate (which must stay fast + offline-friendly); run
# them explicitly when touching an SFC fixture. cartog's own SFC parse/extract is
# covered by the unit tests in crates/cartog-languages/src/sfc.rs.
check-vue: ## Type-check Vue fixtures (vue-tsc via npx, falls back to Docker)
	@echo "==> Checking Vue fixtures..."
	$(call check_lang,npx,node:22-slim,\
		cd benchmarks/fixtures/webapp_vue && npx --yes --package vue --package typescript@5 --package vue-tsc -- vue-tsc --noEmit -p tsconfig.json,\
		cd webapp_vue && HOME=/tmp npm_config_cache=/tmp/npm npx --yes --package vue --package typescript@5 --package vue-tsc -- vue-tsc --noEmit -p tsconfig.json)

check-svelte: ## Type-check Svelte fixtures (svelte-check via npx, falls back to Docker)
	@echo "==> Checking Svelte fixtures..."
	$(call check_lang,npx,node:22-slim,\
		cd benchmarks/fixtures/webapp_svelte && npx --yes --package svelte --package typescript@5 --package svelte-check -- svelte-check --no-tsconfig,\
		cd webapp_svelte && HOME=/tmp npm_config_cache=/tmp/npm npx --yes --package svelte --package typescript@5 --package svelte-check -- svelte-check --no-tsconfig)

check-astro: ## Type-check Astro fixtures (astro check via npx, falls back to Docker)
	@echo "==> Checking Astro fixtures..."
	$(call check_lang,npx,node:22-slim,\
		cd benchmarks/fixtures/webapp_astro && npx --yes --package astro --package typescript@5 --package @astrojs/check -- astro check,\
		cd webapp_astro && HOME=/tmp npm_config_cache=/tmp/npm npx --yes --package astro --package typescript@5 --package @astrojs/check -- astro check)

# --- Skill tests ---

check-skill: ## Run skill tests (ensure_indexed.sh + update_on_exit.sh + install.sh unit tests)
	@echo "==> Checking skill tests..."
	@bash skills/cartog/tests/test_ensure_indexed.sh
	@bash skills/cartog/tests/test_update_on_exit.sh
	@bash skills/cartog/tests/test_install.sh

eval-skill: ## Run LLM-as-judge skill evaluation (requires claude CLI)
	bash skills/cartog/tests/eval.sh

eval-agents: ## Run LLM-as-judge agent evaluation (requires claude CLI)
	bash agents/tests/eval.sh

# --- Formal verification ---
#
# TLA+ models of the two concurrent protocols (PID-file lock acquire +
# single-writer election/promotion). Each correct spec must pass; a
# regenerated broken variant must fail, proving the spec discriminates.
# Needs a JDK + tla2tools.jar (ships in the TLA+ Toolbox cask). Not in
# `make check`: the jar isn't a default CI dependency. Skips cleanly if
# absent so a contributor without TLA+ tooling isn't blocked.

tla: ## Model-check the TLA+ specs (needs tla2tools.jar; see specs/tla/README.md)
	@echo "==> Model-checking TLA+ specs..."
	@bash specs/tla/run.sh

# Loom exhaustively explores thread interleavings + memory reorderings of the
# in-process concurrency that TLA+ can't see (the promoter role/DB commit
# ordering). Isolated in cartog-loom-models because `--cfg loom` makes tokio
# drop tokio::signal, which cartog-mcp uses. Not in `make check`: a separate
# build profile (slower, recompiles deps under the loom cfg).

loom: ## Model-check in-process concurrency with Loom (cartog-loom-models)
	@echo "==> Loom model-checking..."
	RUSTFLAGS="--cfg loom" cargo test -p cartog-loom-models

# --- Benchmarks ---

bench: ## Run shell benchmark suite (all scenarios, all fixtures)
	./benchmarks/token_savings.sh

bench-resolution: ## Run edge-resolution rate (heuristic + host LSP, all languages; saves a provenance snapshot)
	cargo build --release
	CARTOG=$(CURDIR)/target/release/cartog ./benchmarks/resolution_rate.sh
	CARTOG=$(CURDIR)/target/release/cartog ./benchmarks/resolution_rate.sh --lsp

bench-resolution-docker: ## Run edge-resolution rate with Docker LSP servers (run `make lsp-images` first; errors on a missing image, no host fallback)
	cargo build --release
	CARTOG=$(CURDIR)/target/release/cartog ./benchmarks/resolution_rate.sh --lsp --docker-lsp

bench-memory: ## Guard `cartog serve` idle memory footprint (macOS only; skips elsewhere)
	@if [ "$$(uname -s)" != "Darwin" ]; then \
		echo "bench-memory: skipped (needs macOS \`footprint\`; got $$(uname -s))"; \
	else \
		cargo build --release && \
		CARTOG=$(CURDIR)/target/release/cartog ./benchmarks/idle_memory.sh; \
	fi

bench-resolution-scale: ## Catch super-linear resolution regressions (synthetic N vs 2N repo, asserts time ratio; cf. #110)
	cargo build --release
	CARTOG=$(CURDIR)/target/release/cartog ./benchmarks/resolution_scale.sh

lsp-images: ## Build all per-language Docker LSP images (cartog-lsp-<lang>:stable) from benchmarks/lsp-images/
	@for df in benchmarks/lsp-images/*.Dockerfile; do \
		lang=$$(basename "$$df" .Dockerfile); \
		[ "$$lang" = "kotlinc" ] && continue; \
		echo "building cartog-lsp-$$lang:stable"; \
		docker build -t "cartog-lsp-$$lang:stable" -f "$$df" benchmarks/lsp-images || exit 1; \
	done

bench-criterion: ## Run all ONNX-free criterion benches (queries, per-language indexing, hybrid search)
	cargo bench -p cartog --bench queries
	cargo bench -p cartog-indexer --bench indexing
	cargo bench -p cartog --bench rag_search

bench-onnx: ## Run real-model embed/rerank benches (needs `cartog rag setup`; not run in CI)
	cargo bench -p cartog --bench rag_onnx

bench-rag: ## Run RAG relevancy benchmarks (in-memory + shell scenario 13)
	cargo test --test rag_relevancy -- --nocapture
	cargo build --release
	CARTOG=$(CURDIR)/target/release/cartog ./benchmarks/token_savings.sh --scenario 13

bench-agent: ## Run end-to-end agent-task benchmark (cartog on/off; requires claude CLI)
	cargo build --release
	CARTOG=$(CURDIR)/target/release/cartog ./benchmarks/agent/run.sh

.PHONY: check check-rust check-fixtures check-fixtures-docker check-skill check-py check-ts check-go check-rs check-rb check-java check-php check-dart check-swift check-kt check-install-script sync-install-script bench bench-resolution bench-resolution-docker lsp-images bench-criterion bench-rag bench-onnx bench-agent eval-skill eval-agents

# --- Full integrity check ---

check: check-rust check-fixtures check-skill check-install-script ## Run all integrity checks

# --- Install script sync ---
#
# scripts/install.sh is the canonical copy. site/install.sh must be a
# byte-identical mirror because that's what GitHub Pages serves at
# https://jrollin.github.io/cartog/install.sh. We can't symlink because
# Pages does not reliably resolve symlinks outside the published dir.

check-install-script: ## Verify site/install.sh matches scripts/install.sh
	@echo "==> Checking install.sh mirror..."
	@diff -q scripts/install.sh site/install.sh > /dev/null || \
		( echo "    site/install.sh is out of date — run 'make sync-install-script'"; exit 1 )
	@sh -n scripts/install.sh
	@echo "    OK"

sync-install-script: ## Copy scripts/install.sh into site/install.sh (run after editing the script)
	cp scripts/install.sh site/install.sh

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

check-fixtures: check-py check-ts check-go check-rs check-rb check-java check-php check-dart check-swift check-kt ## Validate all fixture codebases

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

check-kt: ## Validate Kotlin fixtures (kotlinc compile, falls back to Docker)
	@echo "==> Checking Kotlin fixtures..."
	$(call check_lang,kotlinc,zenika/kotlin:1.9-jdk17,\
		cd benchmarks/fixtures/webapp_kt && kotlinc src -include-runtime -d /tmp/webapp_kt.jar,\
		cd webapp_kt && HOME=/tmp kotlinc src -include-runtime -d /tmp/webapp_kt.jar)

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

lsp-images: ## Build all per-language Docker LSP images (cartog-lsp-<lang>:stable) from benchmarks/lsp-images/
	@for df in benchmarks/lsp-images/*.Dockerfile; do \
		lang=$$(basename "$$df" .Dockerfile); \
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

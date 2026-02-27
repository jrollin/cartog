.PHONY: check check-rust check-fixtures check-py check-ts check-go check-rs check-rb bench bench-criterion

# --- Full integrity check ---

check: check-rust check-fixtures ## Run all integrity checks

# --- Rust project checks ---

check-rust: ## cargo fmt + clippy + test
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

# --- Fixture syntax/build checks ---

check-fixtures: check-py check-go check-rs check-rb ## Validate all fixture codebases

check-py: ## Validate Python fixtures (py_compile)
	@echo "==> Checking Python fixtures..."
	@find benchmarks/fixtures/webapp_py -name '*.py' -exec python3 -m py_compile {} +
	@echo "    OK"

check-ts: ## Validate TypeScript fixtures (tsc --noEmit)
	@echo "==> Checking TypeScript fixtures..."
	@cd benchmarks/fixtures/webapp_ts && npx tsc --noEmit --strict --esModuleInterop --skipLibCheck
	@echo "    OK"

check-go: ## Validate Go fixtures (go build)
	@echo "==> Checking Go fixtures..."
	@cd benchmarks/fixtures/webapp_go && go build ./...
	@echo "    OK"

check-rs: ## Validate Rust fixtures (cargo check)
	@echo "==> Checking Rust fixtures..."
	@cd benchmarks/fixtures/webapp_rs && cargo check 2>/dev/null
	@echo "    OK"

check-rb: ## Validate Ruby fixtures (ruby -c)
	@echo "==> Checking Ruby fixtures..."
	@find benchmarks/fixtures/webapp_rb -name '*.rb' -exec ruby -c {} + > /dev/null
	@echo "    OK"

# --- Benchmarks ---

bench: ## Run shell benchmark suite (all scenarios, all fixtures)
	./benchmarks/run.sh

bench-criterion: ## Run Rust criterion benchmarks (query latency)
	cargo bench --bench queries

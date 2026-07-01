.DEFAULT_GOAL := help

# Line-coverage floor for `make cov`; see docs/design/test-coverage-gate.md.
COVERAGE_FLOOR := 70

.PHONY: help build check test fix cov cov-html cov-collect

help: ## List available targets
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  %-8s %s\n", $$1, $$2}'

build: ## Build the binary
	cargo build

check: ## Run formatter, linter, supply-chain, and dedup gates
	nix fmt
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo clippy --all-targets --all-features -- -D warnings
	GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null cargo deny check
	cargo machete
	cpd --no-tips --no-colors .
	cargo dupes check --exclude-tests --min-nodes 25 --max-exact 0 --max-near 0

test: ## Run the test suite (default + all features)
	cargo test
	cargo test --all-features

fix: ## Apply the fixable variants of the check gates
	cargo fmt
	cargo clippy --all-targets --fix --allow-dirty --allow-staged
	cargo machete --fix

cov: cov-collect ## Run the test suite under coverage and enforce the floor
	cargo llvm-cov report --summary-only --fail-under-lines $(COVERAGE_FLOOR)

cov-html: cov-collect ## Run the test suite under coverage and write an HTML report
	cargo llvm-cov report --html

# Instrument and run both test configurations, accumulating profile data without
# emitting a report; `cov`/`cov-html` then merge it.
cov-collect:
	cargo llvm-cov clean --workspace
	cargo llvm-cov --no-report
	cargo llvm-cov --no-report --all-features
